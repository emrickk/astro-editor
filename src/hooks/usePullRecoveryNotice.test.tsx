import { act, render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PullRecovery } from '@/lib/bindings'
import { useProjectStore } from '@/store/projectStore'
import { usePublishStore } from '@/store/publishStore'
import { usePullRecoveryNotice } from './usePullRecoveryNotice'

interface NoticeOptions {
  id: string
  action: {
    label: string
    onClick: () => void
  }
}

const toastMock = vi.hoisted(() => ({
  warning: vi.fn<(message: string, options: NoticeOptions) => void>(),
  dismiss: vi.fn<(id: string) => void>(),
}))

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

function NoticeHarness({ onReview }: { onReview: () => void }) {
  usePullRecoveryNotice(onReview)
  return null
}

describe('usePullRecoveryNotice', () => {
  beforeEach(() => {
    globalThis.mockTauri.reset()
    Object.values(toastMock).forEach(mock => mock.mockReset())
    useProjectStore.setState({ projectPath: null })
    usePublishStore.setState({ stage: 'idle' })
  })

  afterEach(() => {
    useProjectStore.setState({ projectPath: null })
    usePublishStore.setState({ stage: 'idle' })
  })

  it('offers an action that opens the recovery settings directly', async () => {
    const onReview = vi.fn()
    globalThis.mockTauri.invoke.mockResolvedValue([recovery])
    useProjectStore.setState({ projectPath: '/repo' })
    const { unmount } = render(<NoticeHarness onReview={onReview} />)

    await waitFor(() => expect(toastMock.warning).toHaveBeenCalledOnce())
    const options = toastMock.warning.mock.calls[0]?.[1]
    expect(options?.id).toBe('pull-recoveries:/repo')
    expect(options?.action.label).toBe('Review copies')

    options?.action.onClick()
    expect(onReview).toHaveBeenCalledOnce()

    act(() => useProjectStore.setState({ projectPath: '/other' }))
    options?.action.onClick()
    expect(onReview).toHaveBeenCalledOnce()

    unmount()
    expect(toastMock.dismiss).toHaveBeenCalledWith('pull-recoveries:/repo')
  })

  it('ignores stale results, dismisses the old notice, and checks again on return', async () => {
    let finishOld: ((items: PullRecovery[]) => void) | undefined
    let oldRequests = 0
    globalThis.mockTauri.invoke.mockImplementation(
      (_command: string, args?: unknown) => {
        const projectPath = (args as { projectPath: string }).projectPath
        if (projectPath === '/old') {
          oldRequests += 1
          if (oldRequests === 1) {
            return new Promise(resolve => {
              finishOld = resolve
            })
          }
          return Promise.resolve([recovery])
        }
        return Promise.resolve([])
      }
    )
    useProjectStore.setState({ projectPath: '/old' })
    render(<NoticeHarness onReview={vi.fn()} />)
    await waitFor(() => expect(oldRequests).toBe(1))

    act(() => useProjectStore.setState({ projectPath: '/new' }))
    expect(toastMock.dismiss).toHaveBeenCalledWith('pull-recoveries:/old')
    act(() => finishOld?.([recovery]))
    await Promise.resolve()
    expect(toastMock.warning).not.toHaveBeenCalled()

    act(() => useProjectStore.setState({ projectPath: '/old' }))
    await waitFor(() => expect(oldRequests).toBe(2))
    await waitFor(() =>
      expect(toastMock.warning).toHaveBeenCalledWith(
        '1 Pull safety copy available',
        expect.objectContaining({ id: 'pull-recoveries:/old' })
      )
    )
  })

  it('checks the same project again after Pull finishes', async () => {
    let listRequests = 0
    const needsAttention: PullRecovery = {
      ...recovery,
      id: 'pull-urgent',
      status: 'needsAttention',
    }
    globalThis.mockTauri.invoke.mockImplementation(() => {
      listRequests += 1
      return Promise.resolve(listRequests === 1 ? [] : [needsAttention])
    })
    useProjectStore.setState({ projectPath: '/repo' })
    render(<NoticeHarness onReview={vi.fn()} />)
    await waitFor(() => expect(listRequests).toBe(1))

    act(() => usePublishStore.setState({ stage: 'pulling' }))
    act(() => usePublishStore.setState({ stage: 'idle' }))

    await waitFor(() => expect(listRequests).toBe(2))
    await waitFor(() =>
      expect(toastMock.warning).toHaveBeenCalledWith(
        '1 Pull safety copy needs attention',
        expect.objectContaining({ id: 'pull-recoveries:/repo' })
      )
    )
  })
})
