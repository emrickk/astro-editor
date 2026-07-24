import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { FileEntry } from '@/types'
import { useEditorStore } from './editorStore'
import { useProjectStore } from './projectStore'
import { usePublishStore } from './publishStore'
import {
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from './projectOperationLease'

const toastMock = vi.hoisted(() => ({
  loading: vi.fn(),
  success: vi.fn(),
  warning: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  dismiss: vi.fn(),
}))
const queryClientMock = vi.hoisted(() => ({
  invalidateQueries: vi.fn().mockResolvedValue(undefined),
  setQueryData: vi.fn(),
}))

vi.mock('../lib/toast', () => ({ toast: toastMock }))
vi.mock('../lib/query-client', () => ({
  queryClient: queryClientMock,
}))

const file: FileEntry = {
  id: 'post',
  name: 'post.md',
  path: '/repo/src/content/posts/post.md',
  extension: 'md',
  collection: 'posts',
  last_modified: null,
  frontmatter: null,
}

const preflightOutput = `post change(s) vs origin/main (1):
  src/content/posts/post.md
changeset digest: abc123`

const parsedPost = {
  content: 'body from disk',
  frontmatter: { title: 'Post' },
  raw_frontmatter: 'title: Post',
  imports: '',
}

function withParsedPost(
  implementation: (command: string, args?: unknown) => Promise<unknown>
) {
  return (command: string, args?: unknown) =>
    command === 'parse_markdown_content'
      ? Promise.resolve(parsedPost)
      : implementation(command, args)
}

function resetStores(overrides: Record<string, unknown> = {}) {
  usePublishStore.setState({
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
  })
  useEditorStore.setState({
    currentFile: file,
    editorContent: 'body',
    frontmatter: {},
    rawFrontmatter: '',
    imports: '',
    isDirty: false,
    isFrontmatterDirty: false,
    autoSaveTimeoutId: null,
    autoSaveCallback: vi.fn().mockResolvedValue(undefined),
    isOperationLocked: false,
  })
  useProjectStore.setState({
    projectPath: '/repo',
    isOperationLocked: false,
    currentProjectSettings: {
      pathOverrides: { contentDirectory: 'src/content/' },
      frontmatterMappings: {},
      publishPreflightCommand: 'ship --preflight --only {files}',
      publishConfirmCommand: 'ship --yes --digest {digest} --only {files}',
      ...overrides,
    },
  })
}

describe('publish store transaction safety', () => {
  beforeEach(() => {
    resetProjectOperationLeaseForTests()
    globalThis.mockTauri.reset()
    globalThis.mockTauri.listen.mockResolvedValue(vi.fn())
    Object.values(toastMock).forEach(mock => mock.mockReset())
    queryClientMock.invalidateQueries.mockReset().mockResolvedValue(undefined)
    queryClientMock.setQueryData.mockReset()
    resetStores()
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('refuses pull while another project operation owns the lease', async () => {
    const imageLease = tryAcquireProjectOperation('image', '/repo')

    await usePublishStore.getState().pull()

    expect(imageLease).not.toBeNull()
    expect(globalThis.mockTauri.invoke).not.toHaveBeenCalled()
    expect(usePublishStore.getState().stage).toBe('idle')
    expect(toastMock.info).toHaveBeenCalledWith(
      'Finish the current project operation first',
      expect.objectContaining({ description: 'Image is still running.' })
    )
  })

  it('refuses publish while another project operation owns the lease', async () => {
    const deleteLease = tryAcquireProjectOperation('delete', '/repo')

    await usePublishStore.getState().startPublish()

    expect(deleteLease).not.toBeNull()
    expect(globalThis.mockTauri.invoke).not.toHaveBeenCalled()
    expect(usePublishStore.getState().stage).toBe('idle')
    expect(toastMock.info).toHaveBeenCalledWith(
      'Finish the current project operation first',
      expect.objectContaining({ description: 'Delete is still running.' })
    )
  })

  it('does not run pull when the required save rejects', async () => {
    const save = vi.fn().mockRejectedValue(new Error('disk full'))
    useEditorStore.setState({
      isDirty: true,
      autoSaveCallback: save,
    })

    await usePublishStore.getState().pull()

    expect(save).toHaveBeenCalledOnce()
    expect(globalThis.mockTauri.invoke).not.toHaveBeenCalled()
    expect(usePublishStore.getState().stage).toBe('idle')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
    expect(toastMock.error).toHaveBeenCalledWith(
      'Pull failed',
      expect.objectContaining({ description: 'disk full' })
    )
  })

  it('uses transactional safe pull for the default command', async () => {
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) =>
        Promise.resolve(command === 'safe_git_pull' ? 'Updated main' : null)
      )
    )

    await usePublishStore.getState().pull()

    expect(globalThis.mockTauri.invoke).toHaveBeenCalledWith('safe_git_pull', {
      projectPath: '/repo',
    })
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'run_project_command'
      )
    ).toBe(false)
    expect(toastMock.success).toHaveBeenCalledWith(
      'Pull complete',
      expect.objectContaining({ description: 'Updated main' })
    )
    expect(useEditorStore.getState().editorContent).toBe('body from disk')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('waits for an in-flight save before starting pull', async () => {
    let releaseSave: (() => void) | undefined
    const save = vi.fn(
      () =>
        new Promise<void>(resolve => {
          releaseSave = () => {
            useEditorStore.setState({ isDirty: false })
            resolve()
          }
        })
    )
    useEditorStore.setState({ isDirty: true, autoSaveCallback: save })
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) =>
        Promise.resolve(command === 'safe_git_pull' ? 'Updated main' : null)
      )
    )

    const inFlightSave = useEditorStore.getState().saveFile(false)
    await Promise.resolve()
    const pull = usePublishStore.getState().pull()
    await Promise.resolve()

    expect(save).toHaveBeenCalledOnce()
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'safe_git_pull'
      )
    ).toBe(false)

    releaseSave?.()
    await Promise.all([inFlightSave, pull])

    expect(save).toHaveBeenCalledOnce()
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'safe_git_pull'
      )
    ).toBe(true)
  })

  it('refreshes disk state and blocks on an error after pull starts', async () => {
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) =>
        command === 'safe_git_pull'
          ? Promise.reject(
              new Error(
                'Pull updated the branch but draft verification failed. Recovery: refs/astro-editor/pull-recovery/abc.'
              )
            )
          : Promise.resolve(null)
      )
    )

    await usePublishStore.getState().pull()

    expect(usePublishStore.getState().stage).toBe('error')
    expect(usePublishStore.getState().error).toContain(
      'refs/astro-editor/pull-recovery/abc'
    )
    expect(useEditorStore.getState().editorContent).toBe('body from disk')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('keeps editing locked until the refreshed queries finish', async () => {
    let releaseRefresh: (() => void) | undefined
    queryClientMock.invalidateQueries.mockImplementationOnce(
      () =>
        new Promise<void>(resolve => {
          releaseRefresh = resolve
        })
    )
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) =>
        Promise.resolve(command === 'safe_git_pull' ? 'Updated main' : null)
      )
    )

    const pull = usePublishStore.getState().pull()
    await vi.waitFor(() =>
      expect(queryClientMock.invalidateQueries).toHaveBeenCalled()
    )

    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    releaseRefresh?.()
    await pull
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('keeps a successful Pull outcome when the editor reload fails', async () => {
    queryClientMock.invalidateQueries.mockRejectedValueOnce(
      new Error('query refresh failed')
    )
    globalThis.mockTauri.invoke.mockImplementation((command: string) =>
      Promise.resolve(command === 'safe_git_pull' ? 'Updated main' : null)
    )

    await usePublishStore.getState().pull()

    const state = usePublishStore.getState()
    expect(state.stage).toBe('reload-warning')
    expect(state.completionLabel).toBe('Pull complete')
    expect(state.completionSummary).toBe('Updated main')
    expect(state.reloadWarning).toContain('query refresh failed')
    expect(state.error).toBeNull()
    expect(useEditorStore.getState().currentFile).toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
    expect(toastMock.error).not.toHaveBeenCalledWith(
      'Pull failed',
      expect.anything()
    )
  })

  it('keeps configured custom pull commands on the command runner', async () => {
    resetStores({ pullCommand: 'git pull origin preview' })
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) =>
        Promise.resolve(
          command === 'run_project_command' ? 'Custom pull done' : null
        )
      )
    )

    await usePublishStore.getState().pull()

    expect(globalThis.mockTauri.invoke).toHaveBeenCalledWith(
      'run_project_command',
      {
        command: 'git pull origin preview',
        projectPath: '/repo',
      }
    )
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'safe_git_pull'
      )
    ).toBe(false)
  })

  it('does not run preflight when the required save rejects', async () => {
    const save = vi.fn().mockRejectedValue(new Error('read-only volume'))
    useEditorStore.setState({
      isDirty: true,
      autoSaveCallback: save,
    })

    await usePublishStore.getState().startPublish()

    expect(save).toHaveBeenCalledOnce()
    expect(globalThis.mockTauri.invoke).not.toHaveBeenCalled()
    expect(usePublishStore.getState().stage).toBe('error')
    expect(usePublishStore.getState().error).toBe('read-only volume')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('claims preflight synchronously so a double start runs once', async () => {
    let releaseSave: (() => void) | undefined
    const save = vi.fn(
      () =>
        new Promise<void>(resolve => {
          releaseSave = () => {
            useEditorStore.setState({ isDirty: false })
            resolve()
          }
        })
    )
    useEditorStore.setState({ isDirty: true, autoSaveCallback: save })
    globalThis.mockTauri.invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === 'run_project_command' ? preflightOutput : null
      )
    )

    const first = usePublishStore.getState().startPublish()
    const second = usePublishStore.getState().startPublish()
    await Promise.resolve()

    expect(usePublishStore.getState().stage).toBe('preflight')
    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    expect(useProjectStore.getState().isOperationLocked).toBe(true)
    expect(save).toHaveBeenCalledOnce()
    releaseSave?.()
    await Promise.all([first, second])

    expect(
      globalThis.mockTauri.invoke.mock.calls.filter(
        ([command]) => command === 'run_project_command'
      )
    ).toHaveLength(1)
    expect(usePublishStore.getState().stage).toBe('review')
  })

  it('uses the pinned confirm command after settings change', async () => {
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string, args?: unknown) => {
        if (command !== 'run_project_command') return Promise.resolve(null)
        const projectCommand = String(
          (args as Record<string, unknown> | undefined)?.command
        )
        return Promise.resolve(
          projectCommand.includes('--preflight')
            ? preflightOutput
            : 'pushed main'
        )
      })
    )

    await usePublishStore.getState().startPublish()
    const originalSession = usePublishStore.getState().session
    useProjectStore.setState({
      currentProjectSettings: {
        pathOverrides: { contentDirectory: 'elsewhere' },
        frontmatterMappings: {},
        publishPreflightCommand: 'replacement --preflight',
        publishConfirmCommand: 'replacement --digest {digest}',
      },
    })

    await usePublishStore.getState().approveAndShip()

    const commands = globalThis.mockTauri.invoke.mock.calls
      .filter(([command]) => command === 'run_project_command')
      .map(([, args]) => String((args as Record<string, unknown>).command))
    expect(originalSession?.confirmCommand).toContain('ship --yes')
    expect(commands.at(-1)).toBe(
      "ship --yes --digest abc123 --only 'src/content/posts/post.md'"
    )
    expect(usePublishStore.getState().error).toBeNull()
    expect(usePublishStore.getState().stage).toBe('idle')
  })

  it('keeps a successful Publish outcome when the editor reload fails', async () => {
    queryClientMock.invalidateQueries.mockRejectedValueOnce(
      new Error('query refresh failed')
    )
    globalThis.mockTauri.invoke.mockImplementation(
      (command: string, args?: unknown) => {
        if (command !== 'run_project_command') return Promise.resolve(null)
        const projectCommand = String(
          (args as Record<string, unknown> | undefined)?.command
        )
        return Promise.resolve(
          projectCommand.includes('--preflight')
            ? preflightOutput
            : 'pushed abc123 to origin/main'
        )
      }
    )

    await usePublishStore.getState().startPublish()
    await usePublishStore.getState().approveAndShip()

    const state = usePublishStore.getState()
    expect(state.stage).toBe('reload-warning')
    expect(state.completionLabel).toBe('Pushed, deployment not confirmed')
    expect(state.completionSummary).toBe('pushed abc123 to origin/main')
    expect(state.reloadWarning).toContain('query refresh failed')
    expect(state.error).toBeNull()
    expect(useEditorStore.getState().currentFile).toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('enforces review readiness in the store before confirming', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    const listeners = new Map<string, (event: unknown) => void>()
    globalThis.mockTauri.listen.mockImplementation(
      (event: string, callback: (event: unknown) => void) => {
        listeners.set(event, callback)
        return Promise.resolve(vi.fn())
      }
    )
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string, args?: unknown) => {
        const commandArgs = args as Record<string, unknown> | undefined
        if (command === 'run_project_command') {
          return Promise.resolve(
            String(commandArgs?.command).includes('--preflight')
              ? preflightOutput
              : 'pushed main'
          )
        }
        if (command === 'start_review_server')
          return Promise.resolve('review-42')
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()
    expect(usePublishStore.getState().reviewReady).toBe(false)

    await usePublishStore.getState().approveAndShip()
    expect(usePublishStore.getState().stage).toBe('review')
    expect(toastMock.info).toHaveBeenCalledWith(
      'Wait for the review page to finish building'
    )

    listeners.get('review-server-log')?.({
      payload: {
        id: 'review-42',
        line: 'Review page: http://localhost:4321',
      },
    })
    expect(usePublishStore.getState().reviewReady).toBe(true)

    await usePublishStore.getState().approveAndShip()
    expect(usePublishStore.getState().stage).toBe('idle')
  })

  it('buffers an exit emitted before the review server id returns', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    const listeners = new Map<string, (event: unknown) => void>()
    globalThis.mockTauri.listen.mockImplementation(
      (event: string, callback: (event: unknown) => void) => {
        listeners.set(event, callback)
        return Promise.resolve(vi.fn())
      }
    )
    globalThis.mockTauri.invoke.mockImplementation(
      (command: string, args?: unknown) => {
        const commandArgs = args as Record<string, unknown> | undefined
        if (command === 'run_project_command') {
          return Promise.resolve(
            String(commandArgs?.command).includes('--preflight')
              ? preflightOutput
              : 'pushed main'
          )
        }
        if (command === 'start_review_server') {
          listeners.get('review-server-exit')?.({
            payload: { id: 'review-failed', success: false, code: 1 },
          })
          return Promise.resolve('review-failed')
        }
        return Promise.resolve(null)
      }
    )

    await usePublishStore.getState().startPublish()

    expect(usePublishStore.getState().stage).toBe('error')
    expect(usePublishStore.getState().error).toMatch(
      /review process failed \(exit 1\)/
    )
    expect(usePublishStore.getState().session).toBeNull()
  })

  it('buffers matching review output until the server id returns', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    const listeners = new Map<string, (event: unknown) => void>()
    globalThis.mockTauri.listen.mockImplementation(
      (event: string, callback: (event: unknown) => void) => {
        listeners.set(event, callback)
        return Promise.resolve(vi.fn())
      }
    )
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) => {
        if (command === 'run_project_command')
          return Promise.resolve(preflightOutput)
        if (command === 'start_review_server') {
          listeners.get('review-server-log')?.({
            payload: {
              id: 'review-early',
              line: 'Review page: http://localhost:4321',
            },
          })
          return Promise.resolve('review-early')
        }
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()

    expect(usePublishStore.getState().stage).toBe('review')
    expect(usePublishStore.getState().reviewReady).toBe(true)
    await usePublishStore.getState().cancelReview()
  })

  it('ignores stale events from a previous review server', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    const listeners = new Map<string, (event: unknown) => void>()
    let serverNumber = 0
    globalThis.mockTauri.listen.mockImplementation(
      (event: string, callback: (event: unknown) => void) => {
        listeners.set(event, callback)
        return Promise.resolve(vi.fn())
      }
    )
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) => {
        if (command === 'run_project_command')
          return Promise.resolve(preflightOutput)
        if (command === 'start_review_server') {
          serverNumber += 1
          return Promise.resolve(`review-${serverNumber}`)
        }
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()
    await usePublishStore.getState().cancelReview()
    await usePublishStore.getState().startPublish()

    listeners.get('review-server-log')?.({
      payload: {
        id: 'review-1',
        line: 'Review page: http://localhost:4001',
      },
    })
    listeners.get('review-server-exit')?.({
      payload: { id: 'review-1', success: false, code: 1 },
    })

    expect(usePublishStore.getState().stage).toBe('review')
    expect(usePublishStore.getState().reviewReady).toBe(false)
    expect(usePublishStore.getState().reviewId).toBe('review-2')

    listeners.get('review-server-log')?.({
      payload: {
        id: 'review-2',
        line: 'Review page: http://localhost:4002',
      },
    })
    expect(usePublishStore.getState().reviewReady).toBe(true)
    await usePublishStore.getState().cancelReview()
  })

  it('keeps the project lease until cancellation confirms server exit', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    let finishStop: (() => void) | undefined
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) => {
        if (command === 'run_project_command')
          return Promise.resolve(preflightOutput)
        if (command === 'start_review_server')
          return Promise.resolve('review-slow-stop')
        if (command === 'stop_review_server') {
          return new Promise<null>(resolve => {
            finishStop = () => resolve(null)
          })
        }
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()
    const cancellation = usePublishStore.getState().cancelReview()
    await vi.waitFor(() =>
      expect(usePublishStore.getState().stage).toBe('cancelling')
    )

    expect(usePublishStore.getState().session).not.toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    expect(tryAcquireProjectOperation('pull', '/repo')).toBeNull()

    finishStop?.()
    await cancellation
    expect(usePublishStore.getState().stage).toBe('idle')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('keeps the session locked and allows retry when cancellation fails', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    let stopFails = true
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string) => {
        if (command === 'run_project_command')
          return Promise.resolve(preflightOutput)
        if (command === 'start_review_server')
          return Promise.resolve('review-retry-stop')
        if (command === 'stop_review_server') {
          return stopFails
            ? Promise.reject(new Error('force kill not confirmed'))
            : Promise.resolve(null)
        }
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()
    await usePublishStore.getState().cancelReview()

    expect(usePublishStore.getState().stage).toBe('cancelling')
    expect(usePublishStore.getState().error).toContain(
      'force kill not confirmed'
    )
    expect(usePublishStore.getState().session).not.toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    expect(tryAcquireProjectOperation('pull', '/repo')).toBeNull()

    stopFails = false
    await usePublishStore.getState().cancelReview()
    expect(usePublishStore.getState().stage).toBe('idle')
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
  })

  it('does not publish until review shutdown is confirmed', async () => {
    resetStores({ publishReviewCommand: 'preview {files}' })
    const listeners = new Map<string, (event: unknown) => void>()
    let stopFails = true
    let projectCommandRuns = 0
    globalThis.mockTauri.listen.mockImplementation(
      (event: string, callback: (event: unknown) => void) => {
        listeners.set(event, callback)
        return Promise.resolve(vi.fn())
      }
    )
    globalThis.mockTauri.invoke.mockImplementation(
      withParsedPost((command: string, args?: unknown) => {
        if (command === 'run_project_command') {
          projectCommandRuns += 1
          const projectCommand = String(
            (args as Record<string, unknown> | undefined)?.command
          )
          return Promise.resolve(
            projectCommand.includes('--preflight')
              ? preflightOutput
              : 'pushed abc123 to origin/main'
          )
        }
        if (command === 'start_review_server')
          return Promise.resolve('review-before-publish')
        if (command === 'stop_review_server') {
          return stopFails
            ? Promise.reject(new Error('server still alive'))
            : Promise.resolve(null)
        }
        return Promise.resolve(null)
      })
    )

    await usePublishStore.getState().startPublish()
    listeners.get('review-server-log')?.({
      payload: {
        id: 'review-before-publish',
        line: 'Review page: http://localhost:4321',
      },
    })
    await usePublishStore.getState().approveAndShip()

    expect(projectCommandRuns).toBe(1)
    expect(usePublishStore.getState().stage).toBe('review')
    expect(usePublishStore.getState().session).not.toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(true)

    stopFails = false
    await usePublishStore.getState().approveAndShip()
    expect(projectCommandRuns).toBe(2)
    expect(usePublishStore.getState().stage).toBe('idle')
  })

  it('blocks document edits and project switching during a disk operation', () => {
    usePublishStore.setState({ stage: 'shipping' })
    useEditorStore.setState({ isOperationLocked: true })
    useProjectStore.setState({ isOperationLocked: true })

    useEditorStore.getState().setEditorContent('unsafe concurrent edit')
    useEditorStore.getState().updateFrontmatterField('title', 'Changed')
    useProjectStore.getState().setProject('/other-repo')

    expect(useEditorStore.getState().editorContent).toBe('body')
    expect(useEditorStore.getState().frontmatter).toEqual({})
    expect(useProjectStore.getState().projectPath).toBe('/repo')
    expect(toastMock.info).toHaveBeenCalledWith(
      'Finish the current pull or publish first',
      expect.objectContaining({
        description: 'Project switching is temporarily paused.',
      })
    )
  })
})
