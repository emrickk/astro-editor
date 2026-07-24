import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { useEditorStore } from './editorStore'
import { useProjectStore } from './projectStore'
import {
  getActiveProjectOperation,
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from './projectOperationLease'

describe('project operation lease', () => {
  beforeEach(() => {
    resetProjectOperationLeaseForTests()
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('claims synchronously and blocks every competing project operation', () => {
    const lease = tryAcquireProjectOperation('delete', '/project')

    expect(lease).not.toBeNull()
    expect(getActiveProjectOperation()).toEqual({
      kind: 'delete',
      projectPath: '/project',
    })
    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    expect(useProjectStore.getState().isOperationLocked).toBe(true)
    expect(tryAcquireProjectOperation('pull', '/project')).toBeNull()
    expect(tryAcquireProjectOperation('publish', '/other-project')).toBeNull()
  })

  it('only the current lease unlocks interaction and release is idempotent', () => {
    const lease = tryAcquireProjectOperation('restore', '/project')!

    expect(lease.isCurrent()).toBe(true)
    lease.release()
    lease.release()

    expect(lease.isCurrent()).toBe(false)
    expect(getActiveProjectOperation()).toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
    expect(useProjectStore.getState().isOperationLocked).toBe(false)
  })

  it('shares image work with the mutex without blocking its final editor update', () => {
    const lease = tryAcquireProjectOperation('image', '/project')

    expect(lease).not.toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(false)
    expect(useProjectStore.getState().isOperationLocked).toBe(false)
    expect(tryAcquireProjectOperation('pull', '/project')).toBeNull()
    expect(tryAcquireProjectOperation('switch', '/other-project')).toBeNull()
  })

  it('locks interaction while a project switch owns the mutex', () => {
    const lease = tryAcquireProjectOperation('switch', '/other-project')

    expect(lease).not.toBeNull()
    expect(useEditorStore.getState().isOperationLocked).toBe(true)
    expect(useProjectStore.getState().isOperationLocked).toBe(true)
    expect(tryAcquireProjectOperation('image', '/project')).toBeNull()
  })

  it('honors an existing interaction lock owned by legacy code', () => {
    useEditorStore.setState({ isOperationLocked: true })

    expect(tryAcquireProjectOperation('delete', '/project')).toBeNull()
  })
})
