import { create } from 'zustand'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { commands } from '@/types'
import { useEditorStore } from './editorStore'
import { useProjectStore } from './projectStore'
import { queryClient } from '../lib/query-client'
import { toast } from '../lib/toast'
import {
  DEFAULT_PULL_COMMAND,
  parsePreflightOutput,
  confirmCommandFor,
} from '../lib/publish'

export type PublishStage =
  | 'idle'
  | 'pulling'
  | 'preflight'
  | 'review'
  | 'shipping'
  | 'error'

interface PublishState {
  stage: PublishStage
  files: string[]
  digest: string | null
  reviewPid: number | null
  log: string[]
  error: string | null
}

interface PublishActions {
  pull: () => Promise<void>
  startPublish: () => Promise<void>
  approveAndShip: () => Promise<void>
  cancelReview: () => Promise<void>
  dismissError: () => void
}

const initialState: PublishState = {
  stage: 'idle',
  files: [],
  digest: null,
  reviewPid: null,
  log: [],
  error: null,
}

const MAX_LOG_LINES = 200

/** Streams `project-command-log` events into a callback for the duration of
 *  one command run. */
async function withCommandLog(
  onLine: (line: string) => void,
  run: () => Promise<void>
): Promise<void> {
  let unlisten: UnlistenFn | null = null
  try {
    unlisten = await listen<{ line: string }>('project-command-log', event => {
      onLine(event.payload.line)
    })
    await run()
  } finally {
    unlisten?.()
  }
}

async function saveOpenFileIfDirty(): Promise<void> {
  const { currentFile, isDirty, saveFile } = useEditorStore.getState()
  if (currentFile && isDirty) {
    await saveFile(false)
  }
}

function refreshProjectData(): void {
  // A pull (or publish) can change anything: collections, file lists, and
  // open file content. Invalidate everything and let active queries refetch.
  void queryClient.invalidateQueries()
}

async function stopReviewServer(pid: number | null): Promise<void> {
  if (pid === null) return
  const result = await commands.stopReviewServer(pid)
  if (result.status === 'error') {
    // Best-effort: an already-dead server is fine, anything else is only
    // worth a console note, never a blocked publish.
    // eslint-disable-next-line no-console
    console.warn('Could not stop review server:', result.error)
  }
}

export const usePublishStore = create<PublishState & PublishActions>(
  (set, get) => ({
    ...initialState,

    pull: async () => {
      if (get().stage !== 'idle') return
      const { projectPath, currentProjectSettings } =
        useProjectStore.getState()
      if (!projectPath) return

      set({ stage: 'pulling' })
      const command =
        currentProjectSettings?.pullCommand?.trim() || DEFAULT_PULL_COMMAND
      toast.loading('Pulling latest changes…', { id: 'pull' })
      try {
        await saveOpenFileIfDirty()
        let lastLine = ''
        await withCommandLog(
          line => {
            if (line.trim()) {
              lastLine = line.trim()
              toast.loading('Pulling latest changes…', {
                id: 'pull',
                description: lastLine.slice(0, 120),
              })
            }
          },
          async () => {
            const result = await commands.runProjectCommand(
              command,
              projectPath
            )
            if (result.status === 'error') {
              throw new Error(result.error)
            }
            const output = result.data.trim()
            const summary = output.split('\n').filter(Boolean).pop() ?? ''
            toast.success('Pull complete', {
              id: 'pull',
              description: summary.slice(0, 120),
            })
            refreshProjectData()
          }
        )
      } catch (error) {
        toast.error('Pull failed', {
          id: 'pull',
          description:
            error instanceof Error ? error.message : 'Unknown error',
          duration: 10000,
        })
      } finally {
        set({ stage: 'idle' })
      }
    },

    startPublish: async () => {
      if (get().stage !== 'idle') return
      const { projectPath, currentProjectSettings } =
        useProjectStore.getState()
      const preflightCommand =
        currentProjectSettings?.publishPreflightCommand?.trim()
      const confirmCommand =
        currentProjectSettings?.publishConfirmCommand?.trim()
      if (!projectPath || !preflightCommand || !confirmCommand) return

      set({ stage: 'preflight', files: [], digest: null, log: [], error: null })
      toast.loading('Checking what would publish…', { id: 'publish' })
      try {
        await saveOpenFileIfDirty()
        const result = await commands.runProjectCommand(
          preflightCommand,
          projectPath
        )
        if (result.status === 'error') {
          throw new Error(result.error)
        }
        const preflight = parsePreflightOutput(result.data)
        if (preflight.empty || !preflight.digest) {
          toast.info('Nothing to publish', { id: 'publish' })
          set({ ...initialState })
          return
        }

        toast.dismiss('publish')
        let reviewPid: number | null = null
        const reviewCommand =
          currentProjectSettings?.publishReviewCommand?.trim()
        if (reviewCommand) {
          const server = await commands.startReviewServer(
            reviewCommand,
            projectPath
          )
          if (server.status === 'error') {
            throw new Error(server.error)
          }
          reviewPid = server.data
        }

        set({
          stage: 'review',
          files: preflight.files,
          digest: preflight.digest,
          reviewPid,
        })
      } catch (error) {
        toast.error('Publish preflight failed', {
          id: 'publish',
          description:
            error instanceof Error ? error.message : 'Unknown error',
          duration: 12000,
        })
        set({ ...initialState })
      }
    },

    approveAndShip: async () => {
      const { stage, digest, reviewPid } = get()
      if (stage !== 'review' || !digest) return
      const { projectPath, currentProjectSettings } =
        useProjectStore.getState()
      const confirmTemplate =
        currentProjectSettings?.publishConfirmCommand?.trim()
      if (!projectPath || !confirmTemplate) return

      set({ stage: 'shipping', log: [], error: null })
      await stopReviewServer(reviewPid)
      set({ reviewPid: null })

      try {
        await withCommandLog(
          line => {
            if (!line.trim()) return
            set(state => ({
              log: [...state.log.slice(-(MAX_LOG_LINES - 1)), line],
            }))
          },
          async () => {
            const result = await commands.runProjectCommand(
              confirmCommandFor(confirmTemplate, digest),
              projectPath
            )
            if (result.status === 'error') {
              throw new Error(result.error)
            }
            const lines = result.data.split('\n').filter(Boolean)
            const highlight = [...lines]
              .reverse()
              .find(line => /pushed|deploy complete/i.test(line))
            const summary = highlight ?? lines.at(-1) ?? 'done'
            toast.success('Published', {
              description: summary.slice(0, 140),
              duration: 10000,
            })
            refreshProjectData()
            set({ ...initialState })
          }
        )
      } catch (error) {
        set({
          stage: 'error',
          error: error instanceof Error ? error.message : 'Unknown error',
        })
      }
    },

    cancelReview: async () => {
      const { reviewPid } = get()
      await stopReviewServer(reviewPid)
      set({ ...initialState })
    },

    dismissError: () => {
      set({ ...initialState })
    },
  })
)
