import React, { useCallback, useEffect, useRef, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import {
  ArchiveRestore,
  CircleAlert,
  FileClock,
  Loader2,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from 'lucide-react'
import { commands, type PullRecovery } from '@/lib/bindings'
import { toast } from '@/lib/toast'
import {
  getActiveProjectOperation,
  tryAcquireProjectOperation,
  type ProjectOperationLease,
} from '@/store/projectOperationLease'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'

interface PullRecoveryHistoryProps {
  projectPath: string
}

function formatSavedAt(value: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return 'Saved safety copy'
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date)
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function recoveryCountLabel(recovery: PullRecovery): string {
  const count = recovery.files.length
  return `${count} saved ${count === 1 ? 'version' : 'versions'}`
}

function prioritizeRecoveries(recoveries: PullRecovery[]): PullRecovery[] {
  return [...recoveries].sort((left, right) => {
    const attentionOrder =
      Number(right.status === 'needsAttention') -
      Number(left.status === 'needsAttention')
    return attentionOrder || right.createdAt.localeCompare(left.createdAt)
  })
}

function showOperationBusy(): void {
  const active = getActiveProjectOperation()
  toast.info('Finish the current project operation first', {
    description: active
      ? `${active.kind[0]?.toUpperCase()}${active.kind.slice(1)} is still running.`
      : 'Another project operation is still running.',
  })
}

const PullRecoveryHistoryForProject: React.FC<PullRecoveryHistoryProps> = ({
  projectPath,
}) => {
  const [recoveries, setRecoveries] = useState<PullRecovery[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [restoringId, setRestoringId] = useState<string | null>(null)
  const [deletingId, setDeletingId] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<PullRecovery | null>(null)
  const requestIdRef = useRef(0)
  const mountedRef = useRef(true)
  const restoreBusyRef = useRef(false)

  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const refresh = useCallback(async () => {
    const requestId = ++requestIdRef.current
    setLoading(true)
    setLoadError(null)
    try {
      const result = await commands.listPullRecoveries(projectPath)
      if (!mountedRef.current || requestId !== requestIdRef.current) {
        return
      }
      if (result.status === 'error') {
        setLoadError(result.error)
        return
      }
      setRecoveries(prioritizeRecoveries(result.data))
    } catch (error) {
      if (!mountedRef.current || requestId !== requestIdRef.current) {
        return
      }
      setLoadError(
        error instanceof Error ? error.message : 'Safety copies could not load'
      )
    } finally {
      if (mountedRef.current && requestId === requestIdRef.current) {
        setLoading(false)
      }
    }
  }, [projectPath])

  useEffect(() => {
    const timer = window.setTimeout(() => void refresh(), 0)
    return () => {
      window.clearTimeout(timer)
      requestIdRef.current += 1
    }
  }, [refresh])

  const handleRestore = useCallback(
    async (recovery: PullRecovery) => {
      if (restoreBusyRef.current) return
      restoreBusyRef.current = true
      setRestoringId(recovery.id)
      let operationLease: ProjectOperationLease | null = null
      try {
        let selected: string | null
        try {
          selected = await open({
            directory: true,
            multiple: false,
            title: 'Choose a folder for restored copies',
          })
        } catch (error) {
          if (!mountedRef.current) return
          toast.error('Folder picker could not open', {
            description:
              error instanceof Error ? error.message : 'Please try again.',
          })
          return
        }
        if (!selected || !mountedRef.current) return

        operationLease = tryAcquireProjectOperation('restore', projectPath)
        if (!operationLease) {
          showOperationBusy()
          return
        }

        const result = await commands.restorePullRecovery(
          projectPath,
          recovery.id,
          selected
        )
        if (!mountedRef.current) return
        if (result.status === 'error') {
          toast.error('Copies could not be restored', {
            description: result.error,
          })
          return
        }
        toast.success('Safety copies restored', {
          description: `Saved in ${result.data}`,
        })
      } catch (error) {
        if (!mountedRef.current) return
        toast.error('Copies could not be restored', {
          description:
            error instanceof Error ? error.message : 'Please try again.',
        })
      } finally {
        operationLease?.release()
        restoreBusyRef.current = false
        if (mountedRef.current) setRestoringId(null)
      }
    },
    [projectPath]
  )

  const handleDelete = useCallback(async () => {
    const recovery = pendingDelete
    if (!recovery) return
    const operationLease = tryAcquireProjectOperation('delete', projectPath)
    if (!operationLease) {
      showOperationBusy()
      setPendingDelete(null)
      return
    }
    setDeletingId(recovery.id)
    try {
      const result = await commands.deletePullRecovery(projectPath, recovery.id)
      if (!mountedRef.current) return
      if (result.status === 'error') {
        toast.error('Safety copy could not be deleted', {
          description: result.error,
        })
        return
      }
      setRecoveries(current => current.filter(item => item.id !== recovery.id))
      toast.success('Safety copy deleted')
    } catch (error) {
      if (!mountedRef.current) return
      toast.error('Safety copy could not be deleted', {
        description:
          error instanceof Error ? error.message : 'Please try again.',
      })
    } finally {
      operationLease.release()
      if (mountedRef.current) {
        setDeletingId(null)
        setPendingDelete(null)
      }
    }
  }, [pendingDelete, projectPath])

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-4 rounded-lg border bg-muted/35 p-4">
        <div className="flex min-w-0 gap-3">
          <FileClock className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
          <div className="space-y-1">
            <p className="text-sm font-medium">Saved during a cautious Pull</p>
            <p className="text-sm leading-relaxed text-muted-foreground">
              If a file changes while Pull is finishing, Astro Editor keeps the
              newest copy here. Restore places copies in a folder you choose and
              never overwrites the project.
            </p>
          </div>
        </div>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => void refresh()}
          disabled={loading}
          aria-label="Refresh safety copies"
          title="Refresh safety copies"
        >
          <RefreshCw className={loading ? 'animate-spin' : undefined} />
        </Button>
      </div>

      {loading && recoveries.length === 0 && (
        <div className="flex items-center gap-2 rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          Checking for safety copies…
        </div>
      )}

      {!loading && loadError && (
        <div
          role="alert"
          className="rounded-lg border border-destructive/30 p-4"
        >
          <p className="text-sm font-medium">Safety copies could not load</p>
          <p className="mt-1 text-sm text-muted-foreground">{loadError}</p>
          <Button
            variant="outline"
            size="sm"
            className="mt-3"
            onClick={() => void refresh()}
          >
            Try again
          </Button>
        </div>
      )}

      {!loading && !loadError && recoveries.length === 0 && (
        <div className="flex items-center gap-3 rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          <ShieldCheck className="size-5 text-green-600 dark:text-green-500" />
          No safety copies are waiting.
        </div>
      )}

      {recoveries.length > 0 && (
        <div className="max-h-80 space-y-3 overflow-y-auto pr-1">
          {recoveries.map(recovery => (
            <article
              key={recovery.id}
              className={
                recovery.status === 'needsAttention'
                  ? 'rounded-lg border border-amber-500/45 bg-amber-500/5 p-4'
                  : 'rounded-lg border bg-background p-4'
              }
            >
              <div className="flex items-start justify-between gap-3">
                <div>
                  {recovery.status === 'needsAttention' ? (
                    <h4 className="flex items-center gap-1.5 text-sm font-medium text-amber-800 dark:text-amber-300">
                      <CircleAlert className="size-4" />
                      Needs attention
                    </h4>
                  ) : (
                    <h4 className="text-sm font-medium">
                      {formatSavedAt(recovery.createdAt)}
                    </h4>
                  )}
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {recovery.status === 'needsAttention' && (
                      <>{formatSavedAt(recovery.createdAt)} · </>
                    )}
                    {recoveryCountLabel(recovery)}
                  </p>
                </div>
                <div className="flex shrink-0 gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void handleRestore(recovery)}
                    disabled={restoringId !== null || deletingId !== null}
                  >
                    {restoringId === recovery.id ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <ArchiveRestore />
                    )}
                    {restoringId === recovery.id
                      ? 'Restoring…'
                      : 'Restore copies'}
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-muted-foreground hover:text-destructive"
                    onClick={() => setPendingDelete(recovery)}
                    disabled={restoringId !== null || deletingId !== null}
                    aria-label={`Delete safety copy from ${formatSavedAt(recovery.createdAt)}`}
                    title="Delete safety copy"
                  >
                    <Trash2 />
                  </Button>
                </div>
              </div>

              {recovery.status === 'needsAttention' && (
                <p className="mt-3 rounded-md bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-900 dark:text-amber-200">
                  Pull stopped before this copy could be sealed. Restore it to
                  inspect the saved version before deciding whether to delete
                  it.
                </p>
              )}

              <ul className="mt-3 space-y-1.5 border-t pt-3">
                {recovery.files.map(file => (
                  <li
                    key={`${file.relativePath}:${file.sourceTree}`}
                    className="flex items-center justify-between gap-3 text-xs"
                  >
                    <span
                      className="min-w-0 truncate font-mono"
                      title={file.relativePath}
                    >
                      {file.relativePath}
                    </span>
                    <span className="shrink-0 text-muted-foreground">
                      {formatSize(file.size)}
                    </span>
                  </li>
                ))}
              </ul>
            </article>
          ))}
        </div>
      )}

      <AlertDialog
        open={pendingDelete !== null}
        onOpenChange={open => {
          if (!open && deletingId === null) setPendingDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this safety copy?</AlertDialogTitle>
            <AlertDialogDescription>
              These saved versions will be permanently removed. Your current
              project files will not change.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deletingId !== null}>
              Keep it
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={() => void handleDelete()}
              disabled={deletingId !== null}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deletingId ? 'Deleting…' : 'Delete permanently'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

export const PullRecoveryHistory: React.FC<
  PullRecoveryHistoryProps
> = props => (
  <PullRecoveryHistoryForProject key={props.projectPath} {...props} />
)
