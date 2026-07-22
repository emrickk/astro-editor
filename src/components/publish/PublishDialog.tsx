import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog'
import { Button } from '../ui/button'
import { Badge } from '../ui/badge'
import { Check, Circle, Loader2, Rocket } from 'lucide-react'
import { cn } from '../../lib/utils'
import { usePublishStore, SHIP_PHASES } from '../../store/publishStore'

/**
 * Publish confirmation dialog.
 *
 * Opens after a successful preflight: lists the files that would ship and
 * waits for the owner's decision while the review server (if configured)
 * builds and opens the production preview in the browser. Approving runs
 * the project's confirm command with a phase indicator and live log;
 * failures (including the pipeline refusing at preflight) keep their full
 * explanation on screen.
 */
export function PublishDialog() {
  const stage = usePublishStore(state => state.stage)
  const files = usePublishStore(state => state.files)
  const digest = usePublishStore(state => state.digest)
  const reviewReady = usePublishStore(state => state.reviewReady)
  const reviewProgress = usePublishStore(state => state.reviewProgress)
  const shipPhase = usePublishStore(state => state.shipPhase)
  const log = usePublishStore(state => state.log)
  const error = usePublishStore(state => state.error)
  const approveAndShip = usePublishStore(state => state.approveAndShip)
  const cancelReview = usePublishStore(state => state.cancelReview)
  const dismissError = usePublishStore(state => state.dismissError)

  const open = stage === 'review' || stage === 'shipping' || stage === 'error'
  if (!open) return null

  const handleOpenChange = (next: boolean) => {
    if (next) return
    if (stage === 'review') void cancelReview()
    if (stage === 'error') dismissError()
    // 'shipping' ignores dismissal: the pipeline is already running
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="sm:max-w-lg"
        onInteractOutside={event => {
          if (stage === 'shipping') event.preventDefault()
        }}
      >
        <DialogHeader>
          <DialogTitle>
            {stage === 'review' && `Publish ${files.length} file(s)?`}
            {stage === 'shipping' && 'Publishing…'}
            {stage === 'error' && 'Publish stopped'}
          </DialogTitle>
          <DialogDescription>
            {stage === 'review' &&
              'Look over the production preview in your browser, then approve.'}
            {stage === 'shipping' &&
              'Running the publish pipeline. This can take a few minutes.'}
            {stage === 'error' &&
              'The pipeline explained why below. Nothing was pushed unless the log says otherwise.'}
          </DialogDescription>
        </DialogHeader>

        {stage === 'review' && (
          <div className="space-y-3">
            <ul className="text-sm font-mono max-h-48 overflow-y-auto space-y-1">
              {files.map(file => (
                <li key={file} className="truncate">
                  {file}
                </li>
              ))}
            </ul>
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              {reviewReady ? (
                <>
                  <Check className="size-3.5 text-green-600 dark:text-green-500" />
                  <span>Review page opened in your browser.</span>
                </>
              ) : (
                <>
                  <Loader2 className="size-3.5 animate-spin" />
                  <span className="truncate">
                    Building production preview…
                    {reviewProgress ? ` ${reviewProgress}` : ''}
                  </span>
                </>
              )}
            </div>
            {digest && (
              <Badge variant="secondary" title="Changeset digest">
                {digest}
              </Badge>
            )}
          </div>
        )}

        {stage === 'shipping' && (
          <div className="space-y-3">
            <ol className="space-y-1.5">
              {SHIP_PHASES.map((label, index) => (
                <li
                  key={label}
                  className={cn(
                    'flex items-center gap-2 text-sm',
                    index > shipPhase && 'text-muted-foreground'
                  )}
                >
                  {index < shipPhase ? (
                    <Check className="size-4 text-green-600 dark:text-green-500" />
                  ) : index === shipPhase ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Circle className="size-4 opacity-30" />
                  )}
                  {label}
                </li>
              ))}
            </ol>
            {log.length > 0 && (
              <pre className="text-xs font-mono bg-muted rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">
                {log.slice(-10).join('\n')}
              </pre>
            )}
          </div>
        )}

        {stage === 'error' && (
          <div className="space-y-3">
            {error && (
              <pre className="text-sm text-destructive whitespace-pre-wrap font-mono max-h-56 overflow-y-auto">
                {error}
              </pre>
            )}
            {log.length > 0 && (
              <pre className="text-xs font-mono bg-muted rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">
                {log.slice(-14).join('\n')}
              </pre>
            )}
          </div>
        )}

        <DialogFooter>
          {stage === 'review' && (
            <>
              <Button variant="outline" onClick={() => void cancelReview()}>
                Cancel
              </Button>
              <Button onClick={() => void approveAndShip()}>
                <Rocket className="size-4 mr-1" />
                Approve & Ship
              </Button>
            </>
          )}
          {stage === 'shipping' && (
            <Button disabled variant="outline">
              <Loader2 className="size-4 mr-1 animate-spin" />
              Publishing…
            </Button>
          )}
          {stage === 'error' && (
            <Button variant="outline" onClick={dismissError}>
              Close
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
