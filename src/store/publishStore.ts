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
  expandPublishCommand,
  commandWantsFiles,
} from '../lib/publish'
import { getSiblingCandidatePaths } from '../lib/translations'
import { getEffectiveContentDirectory } from '../lib/project-registry'
import { ASTRO_PATHS } from '../lib/constants'

export type PublishStage =
  | 'idle'
  | 'pulling'
  | 'preflight'
  | 'review'
  | 'shipping'
  | 'error'

/** Coarse publish pipeline phases, advanced by known output markers. A
 *  pipeline that prints none of them simply stays on the first phase. */
export const SHIP_PHASES = [
  'Freshness check',
  'Checks',
  'Commit and push',
  'Deploy',
] as const

interface PublishState {
  stage: PublishStage
  files: string[]
  digest: string | null
  reviewPid: number | null
  /** Review server readiness, derived from its output */
  reviewReady: boolean
  /** Last line of review server output (build progress) */
  reviewProgress: string | null
  /** Index into SHIP_PHASES while shipping */
  shipPhase: number
  /** Repo-relative paths the publish is scoped to ({files} expansion) */
  scopedFiles: string[] | null
  /** One-click mode: no dialog while shipping, progress in a toast */
  autoMode: boolean
  log: string[]
  error: string | null
}

interface PublishActions {
  pull: () => Promise<void>
  /** filesOverride: repo-relative paths to publish instead of the open post
   *  (used by the delete flow to publish a removal). */
  startPublish: (filesOverride?: string[]) => Promise<void>
  approveAndShip: () => Promise<void>
  cancelReview: () => Promise<void>
  dismissError: () => void
}

const initialState: PublishState = {
  stage: 'idle',
  files: [],
  digest: null,
  reviewPid: null,
  reviewReady: false,
  reviewProgress: null,
  shipPhase: 0,
  scopedFiles: null,
  autoMode: false,
  log: [],
  error: null,
}

/**
 * Repo-relative paths of the currently open post plus every existing sibling
 * translation: the scope of a per-post publish. Null when no file is open.
 */
async function resolveScopedFiles(
  projectPath: string
): Promise<string[] | null> {
  const { currentFile } = useEditorStore.getState()
  if (!currentFile) return null

  const { currentProjectSettings } = useProjectStore.getState()
  const contentDirectory = getEffectiveContentDirectory(currentProjectSettings)
  const absolute = [currentFile.path]
  for (const candidate of getSiblingCandidatePaths(currentFile.path)) {
    const result = await commands.resolveFileEntry(
      candidate,
      projectPath,
      contentDirectory !== ASTRO_PATHS.CONTENT_DIR ? contentDirectory : null
    )
    if (result.status === 'ok' && result.data) {
      absolute.push(result.data.path)
    }
  }
  const prefix = projectPath.endsWith('/') ? projectPath : `${projectPath}/`
  return absolute.map(p => (p.startsWith(prefix) ? p.slice(prefix.length) : p))
}

const MAX_LOG_LINES = 200

/** Output markers that advance the shipping phase indicator */
const PHASE_MARKERS: Array<{ pattern: RegExp; phase: number }> = [
  { pattern: /running (release|fast) checks/i, phase: 1 },
  { pattern: /VERDICT: GO|fast checks passed/i, phase: 2 },
  { pattern: /^pushed \w+/i, phase: 3 },
]

/** The review server is considered ready once it prints its page or URL */
const REVIEW_READY_RE = /review page:|https?:\/\/localhost/i

// Review server log subscription lives for the whole review stage, outside
// any single command run, so it is tracked at module level.
let reviewLogUnlisten: UnlistenFn | null = null

function stopReviewLogListener(): void {
  reviewLogUnlisten?.()
  reviewLogUnlisten = null
}

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

    startPublish: async (filesOverride?: string[]) => {
      if (get().stage !== 'idle') return
      const { projectPath, currentProjectSettings } =
        useProjectStore.getState()
      const preflightCommand =
        currentProjectSettings?.publishPreflightCommand?.trim()
      const confirmCommand =
        currentProjectSettings?.publishConfirmCommand?.trim()
      if (!projectPath || !preflightCommand || !confirmCommand) return

      // Per-post scoping: when the commands take {files}, publishing is
      // limited to the currently open post and its translation siblings,
      // unless the caller scoped it explicitly (e.g. a deletion).
      let scopedFiles: string[] | null = filesOverride ?? null
      if (
        !scopedFiles &&
        (commandWantsFiles(preflightCommand) ||
          commandWantsFiles(confirmCommand))
      ) {
        scopedFiles = await resolveScopedFiles(projectPath)
        if (!scopedFiles) {
          toast.info('Open the post you want to publish first')
          return
        }
      }

      set({
        stage: 'preflight',
        files: [],
        digest: null,
        scopedFiles,
        log: [],
        error: null,
      })
      toast.loading('Checking what would publish…', { id: 'publish' })
      try {
        await saveOpenFileIfDirty()
        const result = await commands.runProjectCommand(
          expandPublishCommand(preflightCommand, {
            files: scopedFiles ?? undefined,
          }),
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

        // One-click mode: no review server, no confirmation dialog; ship
        // immediately with progress in the toast. The automated gates
        // (purity, freshness, release checks) all still run.
        if (currentProjectSettings?.publishAutoConfirm) {
          set({
            files: preflight.files,
            digest: preflight.digest,
            scopedFiles,
            autoMode: true,
          })
          toast.loading('Publishing…', {
            id: 'publish',
            description: `${preflight.files.length} file(s)`,
          })
          await get().approveAndShip()
          return
        }

        toast.dismiss('publish')
        let reviewPid: number | null = null
        const reviewCommand =
          currentProjectSettings?.publishReviewCommand?.trim()
        if (reviewCommand) {
          // Subscribe before starting so early output lines are not missed
          stopReviewLogListener()
          reviewLogUnlisten = await listen<{ line: string }>(
            'review-server-log',
            event => {
              const line = event.payload.line.trim()
              if (!line) return
              set(state => ({
                reviewProgress: line,
                reviewReady: state.reviewReady || REVIEW_READY_RE.test(line),
              }))
            }
          )
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
          // No review server configured means there is nothing to wait for
          reviewReady: !reviewCommand,
        })
      } catch (error) {
        // The pipeline's own refusal (branch not clean, non-post changes,
        // origin ahead) lands here too; the dialog renders it with line
        // breaks intact, which a toast cannot.
        stopReviewLogListener()
        toast.dismiss('publish')
        set({
          ...initialState,
          stage: 'error',
          error: error instanceof Error ? error.message : 'Unknown error',
        })
      }
    },

    approveAndShip: async () => {
      const { stage, digest, reviewPid, scopedFiles, autoMode } = get()
      // 'review' is the dialog's approve; 'preflight' is the one-click path
      if ((stage !== 'review' && stage !== 'preflight') || !digest) return
      const { projectPath, currentProjectSettings } =
        useProjectStore.getState()
      const confirmTemplate =
        currentProjectSettings?.publishConfirmCommand?.trim()
      if (!projectPath || !confirmTemplate) return

      set({ stage: 'shipping', log: [], error: null, shipPhase: 0 })
      stopReviewLogListener()
      await stopReviewServer(reviewPid)
      set({ reviewPid: null })

      try {
        await withCommandLog(
          line => {
            if (!line.trim()) return
            set(state => {
              let phase = state.shipPhase
              for (const marker of PHASE_MARKERS) {
                if (marker.phase > phase && marker.pattern.test(line)) {
                  phase = marker.phase
                }
              }
              return {
                log: [...state.log.slice(-(MAX_LOG_LINES - 1)), line],
                shipPhase: phase,
              }
            })
            if (autoMode) {
              const { shipPhase } = get()
              toast.loading(`Publishing… ${SHIP_PHASES[shipPhase]}`, {
                id: 'publish',
                description: line.trim().slice(0, 120),
              })
            }
          },
          async () => {
            const result = await commands.runProjectCommand(
              expandPublishCommand(confirmTemplate, {
                digest,
                files: scopedFiles ?? undefined,
              }),
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
              id: 'publish',
              description: summary.slice(0, 140),
              duration: 10000,
            })
            refreshProjectData()
            set({ ...initialState })
          }
        )
      } catch (error) {
        toast.dismiss('publish')
        set({
          stage: 'error',
          error: error instanceof Error ? error.message : 'Unknown error',
        })
      }
    },

    cancelReview: async () => {
      const { reviewPid } = get()
      stopReviewLogListener()
      await stopReviewServer(reviewPid)
      set({ ...initialState })
    },

    dismissError: () => {
      stopReviewLogListener()
      set({ ...initialState })
    },
  })
)
