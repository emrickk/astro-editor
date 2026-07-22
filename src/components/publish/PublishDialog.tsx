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
import { Loader2, Rocket } from 'lucide-react'
import { usePublishStore } from '../../store/publishStore'

/**
 * Publish confirmation dialog.
 *
 * Opens after a successful preflight: lists the files that would ship and
 * waits for the owner's decision while the review server (if configured)
 * builds and opens the production preview in the browser. Approving runs
 * the project's confirm command with a live log; failures keep the log on
 * screen.
 */
export function PublishDialog() {
  const stage = usePublishStore(state => state.stage)
  const files = usePublishStore(state => state.files)
  const digest = usePublishStore(state => state.digest)
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
            {stage === 'error' && 'Publish failed'}
          </DialogTitle>
          <DialogDescription>
            {stage === 'review' &&
              'The production preview is building and the review page will open in your browser shortly. Approve once you have looked it over.'}
            {stage === 'shipping' &&
              'Running release checks, committing, and pushing. This can take a few minutes.'}
            {stage === 'error' &&
              'Nothing was pushed unless the log below says otherwise.'}
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
            {digest && (
              <Badge variant="secondary" title="Changeset digest">
                {digest}
              </Badge>
            )}
          </div>
        )}

        {(stage === 'shipping' || stage === 'error') && (
          <div className="space-y-3">
            {error && (
              <p className="text-sm text-destructive whitespace-pre-wrap">
                {error}
              </p>
            )}
            {log.length > 0 && (
              <pre className="text-xs font-mono bg-muted rounded p-2 max-h-48 overflow-y-auto whitespace-pre-wrap">
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
