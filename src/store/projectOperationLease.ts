import { useEditorStore } from './editorStore'
import { useProjectStore } from './projectStore'

export type ProjectOperationKind =
  | 'pull'
  | 'publish'
  | 'delete'
  | 'restore'
  | 'switch'
  | 'image'

export interface ProjectOperationLease {
  readonly kind: ProjectOperationKind
  readonly projectPath: string
  isCurrent: () => boolean
  release: () => void
}

export interface ActiveProjectOperation {
  readonly kind: ProjectOperationKind
  readonly projectPath: string
}

interface OwnedProjectOperation extends ActiveProjectOperation {
  token: symbol
  locksInteraction: boolean
}

let activeOperation: OwnedProjectOperation | null = null

function setProjectOperationLocks(locked: boolean): void {
  useEditorStore.setState({ isOperationLocked: locked })
  useProjectStore.setState({ isOperationLocked: locked })
}

/** Returns the operation currently holding the app-wide project mutation lock. */
export function getActiveProjectOperation(): ActiveProjectOperation | null {
  if (!activeOperation) return null
  return {
    kind: activeOperation.kind,
    projectPath: activeOperation.projectPath,
  }
}

/**
 * Atomically claims the app-wide project mutation lock before an async
 * workflow yields. The returned lease is the only value allowed to release
 * the lock, and release is safe to call more than once. Image work shares the
 * workflow mutex but leaves editing enabled, so callers must revalidate their
 * captured file before applying the synchronous final editor mutation.
 */
export function tryAcquireProjectOperation(
  kind: ProjectOperationKind,
  projectPath: string
): ProjectOperationLease | null {
  if (
    activeOperation ||
    useEditorStore.getState().isOperationLocked ||
    useProjectStore.getState().isOperationLocked
  ) {
    return null
  }

  const token = Symbol(kind)
  const locksInteraction = kind !== 'image'
  activeOperation = { token, kind, projectPath, locksInteraction }
  if (locksInteraction) setProjectOperationLocks(true)

  let released = false
  return Object.freeze({
    kind,
    projectPath,
    isCurrent: () => !released && activeOperation?.token === token,
    release: () => {
      if (released) return
      released = true
      if (activeOperation?.token !== token) return
      activeOperation = null
      if (locksInteraction) setProjectOperationLocks(false)
    },
  })
}

/** Test-only cleanup for cases that intentionally leave a long-lived lease. */
export function resetProjectOperationLeaseForTests(): void {
  activeOperation = null
  setProjectOperationLocks(false)
}
