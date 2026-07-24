import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { open } from '@tauri-apps/plugin-dialog'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PullRecovery } from '@/lib/bindings'
import {
  getActiveProjectOperation,
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from '@/store/projectOperationLease'
import { PullRecoveryHistory } from './PullRecoveryHistory'

const toastMock = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
}))

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))
vi.mock('@/lib/toast', () => ({ toast: toastMock }))

const recovery: PullRecovery = {
  id: 'completed-1',
  status: 'completed',
  createdAt: '2026-07-23T18:30:00Z',
  recoveryRef: 'refs/astro-editor/pull-recovery/abc',
  snapshot: 'abc123',
  files: [
    {
      relativePath: 'src/content/posts/draft.md',
      sourceTree: 'tree-one',
      size: 2048,
    },
  ],
}

function mockRecoveryCommands(items: PullRecovery[] = [recovery]): void {
  globalThis.mockTauri.invoke.mockImplementation((command: string) => {
    if (command === 'list_pull_recoveries') return Promise.resolve(items)
    if (command === 'restore_pull_recovery')
      return Promise.resolve('/Desktop/Astro Editor Recovery')
    if (command === 'delete_pull_recovery') return Promise.resolve(null)
    return Promise.resolve(null)
  })
}

describe('PullRecoveryHistory', () => {
  beforeEach(() => {
    globalThis.mockTauri.reset()
    vi.mocked(open).mockReset()
    Object.values(toastMock).forEach(mock => mock.mockReset())
    resetProjectOperationLeaseForTests()
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('restores copies to a chosen folder without consuming the safety copy', async () => {
    const user = userEvent.setup()
    mockRecoveryCommands()
    vi.mocked(open).mockResolvedValue('/Desktop')
    render(<PullRecoveryHistory projectPath="/repo" />)

    expect(
      await screen.findByText('src/content/posts/draft.md')
    ).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Restore copies' }))

    await waitFor(() =>
      expect(globalThis.mockTauri.invoke).toHaveBeenCalledWith(
        'restore_pull_recovery',
        {
          projectPath: '/repo',
          recoveryId: 'completed-1',
          destinationDirectory: '/Desktop',
        }
      )
    )
    expect(open).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      title: 'Choose a folder for restored copies',
    })
    expect(toastMock.success).toHaveBeenCalledWith(
      'Safety copies restored',
      expect.objectContaining({
        description: 'Saved in /Desktop/Astro Editor Recovery',
      })
    )
    expect(screen.getByText('src/content/posts/draft.md')).toBeInTheDocument()
    expect(getActiveProjectOperation()).toBeNull()
  })

  it('prioritizes and explains a copy that needs attention', async () => {
    const needsAttention: PullRecovery = {
      ...recovery,
      id: 'pull-urgent',
      status: 'needsAttention',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/urgent.md',
        },
      ],
    }
    mockRecoveryCommands([recovery, needsAttention])
    render(<PullRecoveryHistory projectPath="/repo" />)

    expect(await screen.findByText('Needs attention')).toBeInTheDocument()
    const cards = screen.getAllByRole('article')
    expect(cards[0]).toHaveTextContent('src/content/posts/urgent.md')
    expect(cards[0]).toHaveTextContent(
      'Restore it to inspect the saved version before deciding whether to delete it.'
    )
  })

  it('treats folder cancellation as silent and blocks duplicate pickers', async () => {
    const user = userEvent.setup()
    mockRecoveryCommands()
    let finishPicker: ((path: null) => void) | undefined
    vi.mocked(open).mockReturnValue(
      new Promise(resolve => {
        finishPicker = resolve
      })
    )
    render(<PullRecoveryHistory projectPath="/repo" />)
    const restoreButton = await screen.findByRole('button', {
      name: 'Restore copies',
    })

    await user.click(restoreButton)
    await user.click(restoreButton)

    expect(open).toHaveBeenCalledOnce()
    act(() => finishPicker?.(null))
    await waitFor(() => expect(restoreButton).toBeEnabled())
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'restore_pull_recovery'
      )
    ).toBe(false)
    expect(toastMock.success).not.toHaveBeenCalled()
    expect(toastMock.error).not.toHaveBeenCalled()
  })

  it('does not start a restore while another project operation owns the lease', async () => {
    const user = userEvent.setup()
    mockRecoveryCommands()
    vi.mocked(open).mockResolvedValue('/Desktop')
    render(<PullRecoveryHistory projectPath="/repo" />)
    const restoreButton = await screen.findByRole('button', {
      name: 'Restore copies',
    })
    const pullLease = tryAcquireProjectOperation('pull', '/repo')

    await user.click(restoreButton)

    expect(pullLease).not.toBeNull()
    await waitFor(() =>
      expect(toastMock.info).toHaveBeenCalledWith(
        'Finish the current project operation first',
        expect.objectContaining({ description: 'Pull is still running.' })
      )
    )
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'restore_pull_recovery'
      )
    ).toBe(false)
    pullLease?.release()
  })

  it('requires confirmation before permanently deleting a safety copy', async () => {
    const user = userEvent.setup()
    mockRecoveryCommands()
    render(<PullRecoveryHistory projectPath="/repo" />)
    await screen.findByText('src/content/posts/draft.md')

    await user.click(screen.getByTitle('Delete safety copy'))

    expect(screen.getByText('Delete this safety copy?')).toBeInTheDocument()
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'delete_pull_recovery'
      )
    ).toBe(false)

    await user.click(screen.getByRole('button', { name: 'Delete permanently' }))

    await waitFor(() =>
      expect(globalThis.mockTauri.invoke).toHaveBeenCalledWith(
        'delete_pull_recovery',
        { projectPath: '/repo', recoveryId: 'completed-1' }
      )
    )
    await waitFor(() =>
      expect(
        screen.queryByText('src/content/posts/draft.md')
      ).not.toBeInTheDocument()
    )
    expect(toastMock.success).toHaveBeenCalledWith('Safety copy deleted')
    expect(getActiveProjectOperation()).toBeNull()
  })

  it('ignores an old project list response after the pane switches projects', async () => {
    const oldRecovery = {
      ...recovery,
      id: 'completed-old',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/old-project.md',
        },
      ],
    }
    const newRecovery = {
      ...recovery,
      id: 'completed-new',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/new-project.md',
        },
      ],
    }
    let finishOld: ((items: PullRecovery[]) => void) | undefined
    globalThis.mockTauri.invoke.mockImplementation(
      (command: string, args?: unknown) => {
        if (command !== 'list_pull_recoveries') return Promise.resolve(null)
        const projectPath = (args as { projectPath: string }).projectPath
        if (projectPath === '/old') {
          return new Promise(resolve => {
            finishOld = resolve
          })
        }
        return Promise.resolve([newRecovery])
      }
    )
    const { rerender } = render(<PullRecoveryHistory projectPath="/old" />)

    await waitFor(() =>
      expect(globalThis.mockTauri.invoke).toHaveBeenCalledWith(
        'list_pull_recoveries',
        { projectPath: '/old' }
      )
    )
    rerender(<PullRecoveryHistory projectPath="/new" />)
    expect(
      await screen.findByText('src/content/posts/new-project.md')
    ).toBeInTheDocument()

    act(() => finishOld?.([oldRecovery]))
    await Promise.resolve()
    expect(
      screen.queryByText('src/content/posts/old-project.md')
    ).not.toBeInTheDocument()
  })

  it('clears an old deletion choice when the pane switches projects', async () => {
    const user = userEvent.setup()
    const oldRecovery: PullRecovery = {
      ...recovery,
      id: 'completed-old',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/old-project.md',
        },
      ],
    }
    const newRecovery: PullRecovery = {
      ...recovery,
      id: 'completed-new',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/new-project.md',
        },
      ],
    }
    globalThis.mockTauri.invoke.mockImplementation(
      (command: string, args?: unknown) => {
        if (command !== 'list_pull_recoveries') return Promise.resolve(null)
        const projectPath = (args as { projectPath: string }).projectPath
        return Promise.resolve([
          projectPath === '/old' ? oldRecovery : newRecovery,
        ])
      }
    )
    const { rerender } = render(<PullRecoveryHistory projectPath="/old" />)
    await screen.findByText('src/content/posts/old-project.md')
    await user.click(screen.getByTitle('Delete safety copy'))
    expect(screen.getByText('Delete this safety copy?')).toBeInTheDocument()

    rerender(<PullRecoveryHistory projectPath="/new" />)

    expect(
      await screen.findByText('src/content/posts/new-project.md')
    ).toBeInTheDocument()
    expect(
      screen.queryByText('Delete this safety copy?')
    ).not.toBeInTheDocument()
    expect(
      globalThis.mockTauri.invoke.mock.calls.some(
        ([command]) => command === 'delete_pull_recovery'
      )
    ).toBe(false)
  })

  it('ignores a completed restore after its project pane unmounts', async () => {
    const user = userEvent.setup()
    const newRecovery: PullRecovery = {
      ...recovery,
      id: 'completed-new',
      files: [
        {
          ...recovery.files[0]!,
          relativePath: 'src/content/posts/new-project.md',
        },
      ],
    }
    let finishRestore: ((path: string) => void) | undefined
    globalThis.mockTauri.invoke.mockImplementation(
      (command: string, args?: unknown) => {
        if (command === 'list_pull_recoveries') {
          const projectPath = (args as { projectPath: string }).projectPath
          return Promise.resolve([
            projectPath === '/old' ? recovery : newRecovery,
          ])
        }
        if (command === 'restore_pull_recovery') {
          return new Promise(resolve => {
            finishRestore = resolve
          })
        }
        return Promise.resolve(null)
      }
    )
    vi.mocked(open).mockResolvedValue('/Desktop')
    const { rerender } = render(<PullRecoveryHistory projectPath="/old" />)
    await screen.findByText('src/content/posts/draft.md')
    await user.click(screen.getByRole('button', { name: 'Restore copies' }))
    await waitFor(() => expect(finishRestore).toBeDefined())

    rerender(<PullRecoveryHistory projectPath="/new" />)
    await screen.findByText('src/content/posts/new-project.md')
    act(() => finishRestore?.('/Desktop/Astro Editor Recovery'))

    await waitFor(() => expect(getActiveProjectOperation()).toBeNull())
    expect(toastMock.success).not.toHaveBeenCalled()
  })
})
