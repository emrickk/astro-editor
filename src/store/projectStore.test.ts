import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { projectRegistryManager } from '../lib/project-registry'
import { useEditorStore } from './editorStore'
import {
  getActiveProjectOperation,
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from './projectOperationLease'
import { useProjectStore } from './projectStore'
import type { FileEntry } from '@/types'

const logMocks = vi.hoisted(() => ({
  info: vi.fn().mockResolvedValue(undefined),
  debug: vi.fn().mockResolvedValue(undefined),
  error: vi.fn().mockResolvedValue(undefined),
}))
const toastMock = vi.hoisted(() => ({
  info: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
}))

vi.mock('@tauri-apps/plugin-log', () => logMocks)
vi.mock('../lib/toast', () => ({ toast: toastMock }))

const CURRENT_FILE: FileEntry = {
  id: 'posts/current',
  path: '/current/src/content/posts/current.md',
  name: 'current',
  extension: 'md',
  collection: 'posts',
  last_modified: null,
  frontmatter: null,
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(resolvePromise => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}

describe('project switching operation safety', () => {
  const startFileWatcher = vi.fn().mockResolvedValue(undefined)
  const registry = vi.mocked(projectRegistryManager)

  beforeEach(() => {
    vi.clearAllMocks()
    globalThis.mockTauri.reset()
    resetProjectOperationLeaseForTests()
    startFileWatcher.mockResolvedValue(undefined)
    registry.registerProject.mockImplementation(path =>
      Promise.resolve(path.replace(/^\//, ''))
    )
    registry.getEffectiveSettings.mockResolvedValue({
      pathOverrides: {
        contentDirectory: 'src/content/',
        assetsDirectory: 'src/assets/',
        mdxComponentsDirectory: 'src/components/mdx/',
      },
      frontmatterMappings: {},
    })
    useProjectStore.setState({
      projectPath: '/current',
      currentProjectId: 'current',
      currentProjectSettings: null,
      selectedCollection: 'posts',
      currentSubdirectory: null,
      isOperationLocked: false,
      startFileWatcher,
    })
    useEditorStore.setState({
      currentFile: CURRENT_FILE,
      editorContent: 'draft',
      frontmatter: { title: 'Draft' },
      rawFrontmatter: 'title: Draft',
      imports: '',
      isDirty: false,
      isFrontmatterDirty: false,
      autoSaveTimeoutId: null,
      isOperationLocked: false,
    })
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('claims the shared lease synchronously for the entire switch', async () => {
    const registration = deferred<string>()
    registry.registerProject.mockReturnValue(registration.promise)

    useProjectStore.getState().setProject('/next')

    expect(getActiveProjectOperation()).toEqual({
      kind: 'switch',
      projectPath: '/next',
    })
    expect(tryAcquireProjectOperation('pull', '/current')).toBeNull()
    expect(useEditorStore.getState().currentFile).toBeNull()

    registration.resolve('next')
    await vi.waitFor(() =>
      expect(useProjectStore.getState().projectPath).toBe('/next')
    )
    await vi.waitFor(() => expect(getActiveProjectOperation()).toBeNull())
  })

  it('does not start a switch while image work owns the lease', () => {
    const imageLease = tryAcquireProjectOperation('image', '/current')

    useProjectStore.getState().setProject('/next')

    expect(registry.registerProject.mock.calls).toHaveLength(0)
    expect(useProjectStore.getState().projectPath).toBe('/current')
    expect(useEditorStore.getState().currentFile).toEqual(CURRENT_FILE)
    imageLease?.release()
  })

  it('discards a stale async completion after a newer switch generation', async () => {
    const first = deferred<string>()
    const second = deferred<string>()
    registry.registerProject.mockImplementation(path =>
      path === '/first' ? first.promise : second.promise
    )

    useProjectStore.getState().setProject('/first')
    await vi.waitFor(() =>
      expect(registry.registerProject.mock.calls).toContainEqual(['/first'])
    )

    // Simulate invalidation of an abandoned setup. The generation check must
    // still prevent its later completion from overwriting the newer switch.
    resetProjectOperationLeaseForTests()
    useProjectStore.getState().setProject('/second')
    await vi.waitFor(() =>
      expect(registry.registerProject.mock.calls).toContainEqual(['/second'])
    )

    second.resolve('second')
    await vi.waitFor(() =>
      expect(useProjectStore.getState().projectPath).toBe('/second')
    )
    first.resolve('first')
    await Promise.resolve()
    await Promise.resolve()

    expect(useProjectStore.getState().projectPath).toBe('/second')
    expect(useProjectStore.getState().currentProjectId).toBe('second')
  })
})
