import { create } from 'zustand'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { commands } from '@/types'
import { useEditorStore } from './editorStore'
import { useProjectStore } from './projectStore'
import { queryClient } from '../lib/query-client'
import { toast } from '../lib/toast'
import {
  DEFAULT_PULL_COMMAND,
  classifyPublishCompletion,
  parsePreflightOutput,
  expandPublishCommand,
  commandWantsFiles,
  formatPullError,
  validatePublishCommands,
} from '../lib/publish'
import { getSiblingCandidatePaths } from '../lib/translations'
import { getEffectiveContentDirectory } from '../lib/project-registry'
import { ASTRO_PATHS } from '../lib/constants'
import { queryKeys } from '../lib/query-keys'
import {
  getActiveProjectOperation,
  tryAcquireProjectOperation,
  type ProjectOperationLease,
} from './projectOperationLease'

export type PublishStage =
  | 'idle'
  | 'pulling'
  | 'preflight'
  | 'review'
  | 'cancelling'
  | 'shipping'
  | 'error'
  | 'reload-warning'

/** Stages that must not permit project switching. */
export function isProjectActionLocked(stage: PublishStage): boolean {
  return stage !== 'idle' && stage !== 'error' && stage !== 'reload-warning'
}

export interface PublishSession {
  id: number
  projectPath: string
  preflightCommand: string
  confirmCommand: string
  reviewCommand: string | null
  autoConfirm: boolean
  sourceFilePath: string | null
  contentDirectory: string
  scopedFiles: readonly string[] | null
  digest: string | null
}

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
  reviewId: string | null
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
  errorOperation: 'pull' | 'publish' | null
  /** Proven command outcome retained when only the editor reload fails. */
  completionLabel: string | null
  completionSummary: string | null
  reloadWarning: string | null
  /** Immutable inputs pinned for the lifetime of one publish attempt. */
  session: PublishSession | null
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

interface ReviewServerLogPayload {
  id: string
  line: string
}

interface ReviewServerExitPayload {
  id: string
  success: boolean
  code: number | null
}

type BufferedReviewEvent =
  | { type: 'log'; payload: ReviewServerLogPayload }
  | { type: 'exit'; payload: ReviewServerExitPayload }

const initialState: PublishState = {
  stage: 'idle',
  files: [],
  digest: null,
  reviewId: null,
  reviewReady: false,
  reviewProgress: null,
  shipPhase: 0,
  scopedFiles: null,
  autoMode: false,
  log: [],
  error: null,
  errorOperation: null,
  completionLabel: null,
  completionSummary: null,
  reloadWarning: null,
  session: null,
}

/**
 * Repo-relative paths of the currently open post plus every existing sibling
 * translation: the scope of a per-post publish. The source path and content
 * directory are captured before this asynchronous existence check begins.
 */
async function resolveScopedFiles(
  projectPath: string,
  sourceFilePath: string,
  contentDirectory: string
): Promise<string[]> {
  const absolute = [sourceFilePath]
  for (const candidate of getSiblingCandidatePaths(sourceFilePath)) {
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

let nextSessionId = 1

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
let reviewExitUnlisten: UnlistenFn | null = null
let publishOperationLease: ProjectOperationLease | null = null

function stopReviewListeners(): void {
  reviewLogUnlisten?.()
  reviewLogUnlisten = null
  reviewExitUnlisten?.()
  reviewExitUnlisten = null
}

function releasePublishOperationLease(
  lease: ProjectOperationLease | null = publishOperationLease
): void {
  // A stale async attempt must never release a newer publish attempt's lease.
  if (!lease || publishOperationLease !== lease) return
  lease.release()
  publishOperationLease = null
}

function showOperationBusyToast(): void {
  const active = getActiveProjectOperation()
  toast.info('Finish the current project operation first', {
    description: active
      ? `${active.kind[0]?.toUpperCase()}${active.kind.slice(1)} is still running.`
      : 'Another project operation is still running.',
  })
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
  const { autoSaveTimeoutId } = useEditorStore.getState()
  if (autoSaveTimeoutId) {
    clearTimeout(autoSaveTimeoutId)
    useEditorStore.setState({ autoSaveTimeoutId: null })
  }

  const { currentFile, isDirty, saveFile } = useEditorStore.getState()
  if (currentFile && isDirty) {
    await saveFile(false)
    if (useEditorStore.getState().isDirty) {
      throw new Error(
        'The document changed while it was being saved. The operation was stopped so you can save again.'
      )
    }
  }
}

function clearOpenFileAfterReloadFailure(): void {
  const { currentFile, autoSaveTimeoutId } = useEditorStore.getState()
  if (!currentFile) return
  if (autoSaveTimeoutId) clearTimeout(autoSaveTimeoutId)
  useEditorStore.setState({
    currentFile: null,
    editorContent: '',
    frontmatter: {},
    rawFrontmatter: '',
    imports: '',
    isDirty: false,
    isFrontmatterDirty: false,
    autoSaveTimeoutId: null,
    lastSaveTimestamp: null,
  })
}

async function refreshProjectData(projectPath: string): Promise<void> {
  // Pull can change every query. Keep the editor locked until active queries
  // have refetched, then synchronously replace the open document with the
  // final on-disk bytes so stale pre-pull state can never win a later save.
  try {
    await queryClient.invalidateQueries()
    if (useProjectStore.getState().projectPath !== projectPath) return

    const { currentFile, autoSaveTimeoutId } = useEditorStore.getState()
    if (!currentFile) return
    if (autoSaveTimeoutId) clearTimeout(autoSaveTimeoutId)

    const result = await commands.parseMarkdownContent(
      currentFile.path,
      projectPath
    )
    if (result.status === 'error') throw new Error(result.error)

    queryClient.setQueryData(
      queryKeys.fileContent(projectPath, currentFile.id),
      result.data
    )
    useEditorStore.setState({
      editorContent: result.data.content,
      frontmatter: result.data.frontmatter,
      rawFrontmatter: result.data.raw_frontmatter,
      imports: result.data.imports,
      isDirty: false,
      isFrontmatterDirty: false,
      autoSaveTimeoutId: null,
      lastSaveTimestamp: Date.now(),
    })
  } catch (error) {
    if (useProjectStore.getState().projectPath === projectPath) {
      clearOpenFileAfterReloadFailure()
    }
    const message = error instanceof Error ? error.message : String(error)
    throw new Error(
      `The Git operation completed, but the open file could not be reloaded safely: ${message}`,
      { cause: error }
    )
  }
}

async function stopReviewServer(id: string | null): Promise<void> {
  if (id === null) return
  const result = await commands.stopReviewServer(id)
  if (result.status === 'error') throw new Error(result.error)
}

function reviewStopFailure(error: unknown): string {
  const detail = error instanceof Error ? error.message : String(error)
  return `Nevertheless Editor could not confirm that the review server stopped: ${detail}. Retry before continuing.`
}

export const usePublishStore = create<PublishState & PublishActions>(
  (set, get) => ({
    ...initialState,

    pull: async () => {
      if (get().stage !== 'idle') return
      const { projectPath, currentProjectSettings } = useProjectStore.getState()
      if (!projectPath) return

      const command =
        currentProjectSettings?.pullCommand?.trim() || DEFAULT_PULL_COMMAND
      // Pin the path and command, then claim the operation synchronously. This
      // closes the double-click window before the first save or command await.
      const operationLease = tryAcquireProjectOperation('pull', projectPath)
      if (!operationLease) {
        showOperationBusyToast()
        return
      }
      set({ ...initialState, stage: 'pulling' })
      toast.loading('Pulling latest changes…', { id: 'pull' })
      let commandStarted = false
      let commandSucceeded = false
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
            commandStarted = true
            const result =
              command === DEFAULT_PULL_COMMAND
                ? await commands.safeGitPull(projectPath)
                : await commands.runProjectCommand(command, projectPath)
            if (result.status === 'error') {
              throw new Error(result.error)
            }
            commandSucceeded = true
            const output = result.data.trim()
            const summary = output.split('\n').filter(Boolean).pop() ?? ''
            try {
              await refreshProjectData(projectPath)
            } catch (refreshError) {
              const reloadWarning =
                refreshError instanceof Error
                  ? refreshError.message
                  : String(refreshError)
              toast.warning('Pull complete, editor reload needed', {
                id: 'pull',
                description: summary.slice(0, 120),
                duration: 15000,
              })
              set({
                ...initialState,
                stage: 'reload-warning',
                completionLabel: 'Pull complete',
                completionSummary: summary,
                reloadWarning,
              })
              return
            }
            toast.success('Pull complete', {
              id: 'pull',
              description: summary.slice(0, 120),
            })
          }
        )
      } catch (error) {
        let message = formatPullError(error)
        if (commandStarted && !commandSucceeded) {
          try {
            await refreshProjectData(projectPath)
          } catch (refreshError) {
            const refreshMessage =
              refreshError instanceof Error
                ? refreshError.message
                : String(refreshError)
            message = `${message}\n\n${refreshMessage}`
          }
        }
        toast.error('Pull failed', {
          id: 'pull',
          description: message,
          duration: 15000,
        })
        if (commandStarted) {
          set({
            ...initialState,
            stage: 'error',
            error: message,
            errorOperation: 'pull',
          })
        }
      } finally {
        operationLease.release()
        if (get().stage === 'pulling') {
          set({ ...initialState })
        }
      }
    },

    startPublish: async (filesOverride?: string[]) => {
      if (get().stage !== 'idle') return
      const { projectPath, currentProjectSettings } = useProjectStore.getState()
      const preflightCommand =
        currentProjectSettings?.publishPreflightCommand?.trim()
      const confirmCommand =
        currentProjectSettings?.publishConfirmCommand?.trim()
      if (!projectPath) return

      const validation = validatePublishCommands(
        preflightCommand,
        confirmCommand,
        currentProjectSettings?.publishReviewCommand
      )
      if (!validation.valid || !preflightCommand || !confirmCommand) {
        toast.error('Publish is not configured safely', {
          description: validation.error ?? undefined,
        })
        return
      }

      const wantsFiles = commandWantsFiles(preflightCommand)
      const currentFile = useEditorStore.getState().currentFile
      if (wantsFiles && !filesOverride && !currentFile) {
        toast.info('Open the post you want to publish first')
        return
      }

      const operationLease = tryAcquireProjectOperation('publish', projectPath)
      if (!operationLease) {
        showOperationBusyToast()
        return
      }
      publishOperationLease = operationLease

      const contentDirectory = getEffectiveContentDirectory(
        currentProjectSettings
      )
      let session: PublishSession = Object.freeze({
        id: nextSessionId++,
        projectPath,
        preflightCommand,
        confirmCommand,
        reviewCommand:
          currentProjectSettings?.publishReviewCommand?.trim() || null,
        autoConfirm: Boolean(currentProjectSettings?.publishAutoConfirm),
        sourceFilePath: currentFile?.path ?? null,
        contentDirectory,
        scopedFiles: filesOverride ? Object.freeze([...filesOverride]) : null,
        digest: null,
      })

      // Claim and pin the entire attempt before the first await. All later
      // steps use this session rather than rereading mutable project settings.
      set({
        ...initialState,
        stage: 'preflight',
        session,
        scopedFiles: session.scopedFiles ? [...session.scopedFiles] : null,
      })
      toast.loading('Checking what would publish…', { id: 'publish' })
      let reviewId: string | null = null
      try {
        await saveOpenFileIfDirty()

        if (wantsFiles && !session.scopedFiles) {
          if (!session.sourceFilePath) {
            throw new Error('Open the post you want to publish first.')
          }
          const resolvedFiles = await resolveScopedFiles(
            session.projectPath,
            session.sourceFilePath,
            session.contentDirectory
          )
          if (get().session?.id !== session.id) {
            releasePublishOperationLease(operationLease)
            return
          }
          session = Object.freeze({
            ...session,
            scopedFiles: Object.freeze([...resolvedFiles]),
          })
          set({ session, scopedFiles: [...resolvedFiles] })
        }

        const result = await commands.runProjectCommand(
          expandPublishCommand(session.preflightCommand, {
            files: session.scopedFiles ?? undefined,
          }),
          session.projectPath
        )
        if (get().session?.id !== session.id) {
          releasePublishOperationLease(operationLease)
          return
        }
        if (result.status === 'error') {
          throw new Error(result.error)
        }
        const preflight = parsePreflightOutput(result.data)
        if (preflight.empty || !preflight.digest) {
          toast.info('Nothing to publish', { id: 'publish' })
          releasePublishOperationLease(operationLease)
          set({ ...initialState })
          return
        }

        session = Object.freeze({ ...session, digest: preflight.digest })
        set({
          session,
          files: [...preflight.files],
          digest: preflight.digest,
          scopedFiles: session.scopedFiles ? [...session.scopedFiles] : null,
        })

        // One-click mode: no review server, no confirmation dialog; ship
        // immediately with progress in the toast. The automated gates
        // (purity, freshness, release checks) all still run.
        if (session.autoConfirm) {
          set({ autoMode: true })
          toast.loading('Publishing…', {
            id: 'publish',
            description: `${preflight.files.length} file(s)`,
          })
          await get().approveAndShip()
          return
        }

        toast.dismiss('publish')
        if (session.reviewCommand) {
          // Subscribe before starting so early output lines are not missed
          stopReviewListeners()
          let activeReviewId: string | null = null
          const bufferedEvents: BufferedReviewEvent[] = []

          const handleReviewLog = (payload: ReviewServerLogPayload) => {
            const state = get()
            if (state.session?.id !== session.id) return
            if (state.stage !== 'preflight' && state.stage !== 'review') return
            const line = payload.line.trim()
            if (!line) return
            set(current => ({
              reviewProgress: line,
              reviewReady:
                current.reviewReady || REVIEW_READY_RE.test(payload.line),
            }))
          }

          const handleReviewExit = (payload: ReviewServerExitPayload) => {
            const state = get()
            if (state.session?.id !== session.id) return
            if (state.stage !== 'preflight' && state.stage !== 'review') return

            stopReviewListeners()
            toast.dismiss('publish')
            const code = payload.code === null ? '' : ` (exit ${payload.code})`
            set({
              ...initialState,
              stage: 'error',
              error: payload.success
                ? `The review process exited before approval${code}. Publish was stopped.`
                : `The review process failed${code}. Publish was stopped before approval.`,
              errorOperation: 'publish',
            })
            releasePublishOperationLease(operationLease)
          }

          const routeReviewEvent = (event: BufferedReviewEvent) => {
            if (activeReviewId === null) {
              if (bufferedEvents.length < MAX_LOG_LINES)
                bufferedEvents.push(event)
              return
            }
            if (event.payload.id !== activeReviewId) return
            if (event.type === 'log') handleReviewLog(event.payload)
            else handleReviewExit(event.payload)
          }

          reviewLogUnlisten = await listen<ReviewServerLogPayload>(
            'review-server-log',
            event => {
              routeReviewEvent({ type: 'log', payload: event.payload })
            }
          )
          reviewExitUnlisten = await listen<ReviewServerExitPayload>(
            'review-server-exit',
            event => {
              routeReviewEvent({ type: 'exit', payload: event.payload })
            }
          )
          const server = await commands.startReviewServer(
            expandPublishCommand(session.reviewCommand, {
              digest: session.digest ?? undefined,
              files: session.scopedFiles ?? undefined,
            }),
            session.projectPath
          )
          if (server.status === 'error') {
            throw new Error(server.error)
          }
          reviewId = server.data
          activeReviewId = reviewId
          for (const event of bufferedEvents) routeReviewEvent(event)
          bufferedEvents.length = 0
        }

        if (get().session?.id !== session.id) {
          try {
            await stopReviewServer(reviewId)
            releasePublishOperationLease(operationLease)
          } catch (error) {
            toast.error('Review server is still running', {
              description: reviewStopFailure(error),
            })
          }
          return
        }
        set(current => ({
          stage: 'review',
          reviewId,
          // Preserve readiness if the server emitted its URL before its id
          // was returned. With no review server there is nothing to wait for.
          reviewReady: !session.reviewCommand || current.reviewReady,
        }))
      } catch (error) {
        if (get().session?.id !== session.id) {
          try {
            await stopReviewServer(reviewId)
            releasePublishOperationLease(operationLease)
          } catch (stopError) {
            toast.error('Review server is still running', {
              description: reviewStopFailure(stopError),
            })
          }
          return
        }
        // The pipeline's own refusal (branch not clean, non-post changes,
        // origin ahead) lands here too; the dialog renders it with line
        // breaks intact, which a toast cannot.
        stopReviewListeners()
        try {
          await stopReviewServer(reviewId)
        } catch (stopError) {
          const message = reviewStopFailure(stopError)
          toast.dismiss('publish')
          set({ stage: 'cancelling', reviewId, error: message })
          toast.error('Review server is still running', {
            description: message,
          })
          return
        }
        toast.dismiss('publish')
        releasePublishOperationLease(operationLease)
        set({
          ...initialState,
          stage: 'error',
          error: error instanceof Error ? error.message : 'Unknown error',
          errorOperation: 'publish',
        })
      }
    },

    approveAndShip: async () => {
      const { stage, reviewId, reviewReady, autoMode, session } = get()
      const operationLease = publishOperationLease
      // 'review' is the dialog's approve; 'preflight' is the one-click path
      if ((stage !== 'review' && stage !== 'preflight') || !session?.digest)
        return
      if (stage === 'review' && session.reviewCommand && !reviewReady) {
        toast.info('Wait for the review page to finish building')
        return
      }

      const stopAndShowTerminalError = async (
        message: string,
        releaseLease: boolean
      ): Promise<void> => {
        stopReviewListeners()
        set({ stage: 'cancelling', error: null })
        try {
          await stopReviewServer(reviewId)
        } catch (error) {
          const stopError = reviewStopFailure(error)
          set({ stage: 'cancelling', error: stopError })
          toast.error('Review server is still running', {
            description: stopError,
          })
          return
        }
        if (releaseLease) releasePublishOperationLease(operationLease)
        set({
          ...initialState,
          stage: 'error',
          error: message,
          errorOperation: 'publish',
        })
      }

      if (!operationLease?.isCurrent()) {
        await stopAndShowTerminalError(
          'Publish lost its project safety lock. Nothing was shipped. Start the publish review again.',
          false
        )
        return
      }
      if (useProjectStore.getState().projectPath !== session.projectPath) {
        await stopAndShowTerminalError(
          'The active project changed during review. Publish was stopped before running the confirm command.',
          true
        )
        return
      }

      if (stage === 'review') {
        stopReviewListeners()
        set({ stage: 'cancelling', error: null })
        try {
          await stopReviewServer(reviewId)
        } catch (error) {
          const message = reviewStopFailure(error)
          set({ stage: 'review', error: message })
          toast.error('Review server is still running', {
            description: message,
          })
          return
        }
        if (get().session?.id !== session.id) return
      }

      set({
        stage: 'shipping',
        reviewId: null,
        log: [],
        error: null,
        shipPhase: 0,
      })

      try {
        // Defensive final save assertion before the confirm command. Editing
        // is locked during review, and the pinned digest remains the final
        // freshness guard in the project script.
        await saveOpenFileIfDirty()
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
              expandPublishCommand(session.confirmCommand, {
                digest: session.digest ?? undefined,
                files: session.scopedFiles ?? undefined,
              }),
              session.projectPath
            )
            if (result.status === 'error') {
              throw new Error(result.error)
            }
            const lines = result.data.split('\n').filter(Boolean)
            const completion = classifyPublishCompletion(result.data)
            const reversed = [...lines].reverse()
            const pushed = reversed.find(line => /^pushed\s+\S+/i.test(line))
            const deploymentNote = reversed.find(line =>
              /deploy complete|gh unavailable|deploy watch timed out|check .*actions/i.test(
                line
              )
            )
            const summary = deploymentNote ?? pushed ?? lines.at(-1) ?? 'done'
            const completionLabel =
              completion === 'deployed'
                ? 'Published'
                : completion === 'pushed'
                  ? 'Pushed, deployment not confirmed'
                  : 'Publish command complete'

            try {
              await refreshProjectData(session.projectPath)
            } catch (refreshError) {
              const reloadWarning =
                refreshError instanceof Error
                  ? refreshError.message
                  : String(refreshError)
              toast.warning(`${completionLabel}, editor reload needed`, {
                id: 'publish',
                description: summary.slice(0, 140),
                duration: 15000,
              })
              releasePublishOperationLease(operationLease)
              set({
                ...initialState,
                stage: 'reload-warning',
                completionLabel,
                completionSummary: summary,
                reloadWarning,
              })
              return
            }

            if (completion === 'pushed') {
              toast.warning(completionLabel, {
                id: 'publish',
                description: summary.slice(0, 140),
                duration: 15000,
              })
            } else {
              toast.success(completionLabel, {
                id: 'publish',
                description: summary.slice(0, 140),
                duration: 10000,
              })
            }
            releasePublishOperationLease(operationLease)
            set({ ...initialState })
          }
        )
      } catch (error) {
        toast.dismiss('publish')
        const log = get().log
        releasePublishOperationLease(operationLease)
        set({
          ...initialState,
          stage: 'error',
          error: error instanceof Error ? error.message : 'Unknown error',
          errorOperation: 'publish',
          log,
        })
      }
    },

    cancelReview: async () => {
      const { stage, reviewId, error, session } = get()
      if (stage !== 'review' && stage !== 'cancelling') return
      if (stage === 'cancelling' && error === null) return
      const operationLease = publishOperationLease
      stopReviewListeners()
      set({ stage: 'cancelling', error: null })
      try {
        await stopReviewServer(reviewId)
      } catch (stopError) {
        const message = reviewStopFailure(stopError)
        if (get().session?.id === session?.id && get().reviewId === reviewId) {
          set({ stage: 'cancelling', error: message })
        }
        toast.error('Review server is still running', {
          description: message,
        })
        return
      }
      if (get().session?.id !== session?.id || get().reviewId !== reviewId)
        return
      releasePublishOperationLease(operationLease)
      set({ ...initialState })
    },

    dismissError: () => {
      if (get().stage !== 'error' && get().stage !== 'reload-warning') return
      stopReviewListeners()
      releasePublishOperationLease(publishOperationLease)
      set({ ...initialState })
    },
  })
)
