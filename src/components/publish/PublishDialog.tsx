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
import { Check, Circle, Loader2, Rocket, CircleAlert } from 'lucide-react'
import { cn } from '../../lib/utils'
import { usePublishStore, SHIP_PHASES } from '../../store/publishStore'
import { extractFindings } from '../../lib/publish'

/**
 * Error body: per-file findings first ("what needs fixing"), raw output
 * below for anything the findings parser did not recognize.
 */
function ErrorBody({ error, log }: { error: string | null; log: string[] }) {
  const findings = error ? extractFindings(error) : []
  return (
    <div className="space-y-3">
      {findings.length > 0 && (
        <div className="space-y-2">
          <p className="text-sm font-medium">What needs fixing:</p>
          <ul className="space-y-1.5">
            {findings.map(finding => (
              <li
                key={`${finding.file}|${finding.message}`}
                className="flex items-start gap-2 text-sm"
              >
                <CircleAlert className="size-4 mt-0.5 shrink-0 text-destructive" />
                <span>
                  <span className="font-medium">{finding.message}</span>{' '}
                  <span className="text-muted-foreground font-mono text-xs">
                    {finding.file.split('/').pop()}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {error && (
        <pre
          className={cn(
            'whitespace-pre-wrap font-mono overflow-y-auto',
            findings.length > 0
              ? 'text-xs text-muted-foreground bg-muted rounded p-2 max-h-32'
              : 'text-sm text-destructive max-h-56'
          )}
        >
          {error}
        </pre>
      )}
      {log.length > 0 && findings.length === 0 && (
        <pre className="text-xs font-mono bg-muted rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">
          {log.slice(-14).join('\n')}
        </pre>
      )}
    </div>
  )
}

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
  const autoMode = usePublishStore(state => state.autoMode)
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

  // One-click mode ships without a dialog (progress lives in a toast);
  // only failures surface here.
  const open =
    stage === 'review' ||
    stage === 'error' ||
    (stage === 'shipping' && !autoMode)
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
          <ErrorBody error={error} log={log} />
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
