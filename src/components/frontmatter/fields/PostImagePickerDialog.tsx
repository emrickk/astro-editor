import { useState } from 'react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/dialog'
import { Badge } from '../../ui/badge'
import { useEditorStore } from '../../../store/editorStore'
import {
  extractImageUrls,
  isRenderableImageUrl,
  meetsCoverSpec,
} from '../../../lib/images'

interface PostImagePickerDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onSelect: (url: string) => void
}

/**
 * Picks one of the images already referenced in the post body (remote URLs)
 * to use as the cover. Thumbnails carry their natural size and a warning
 * badge when the image is below the cover size spec, since covers render
 * large on home cards.
 */
export function PostImagePickerDialog({
  open,
  onOpenChange,
  onSelect,
}: PostImagePickerDialogProps) {
  const editorContent = useEditorStore(state =>
    open ? state.editorContent : ''
  )
  const [sizes, setSizes] = useState<
    Record<string, { width: number; height: number }>
  >({})

  const urls = extractImageUrls(editorContent).filter(isRenderableImageUrl)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Choose an image from this post</DialogTitle>
          <DialogDescription>
            The image is downloaded into the project assets and set as the
            cover. Covers render large on the home page: 2400x1260 for wide
            images or 1600x1600 for square ones keeps them sharp.
          </DialogDescription>
        </DialogHeader>
        {urls.length === 0 ? (
          <p className="text-sm text-muted-foreground py-6 text-center">
            This post has no remote images yet.
          </p>
        ) : (
          <div className="grid grid-cols-3 gap-3 max-h-[420px] overflow-y-auto pr-1">
            {urls.map(url => {
              const size = sizes[url]
              const belowSpec =
                size && !meetsCoverSpec(size.width, size.height)
              return (
                <button
                  key={url}
                  type="button"
                  onClick={() => onSelect(url)}
                  className="group relative rounded-md border overflow-hidden text-left focus:outline-none focus:ring-2 focus:ring-ring"
                  title={url}
                >
                  <img
                    src={url}
                    loading="lazy"
                    decoding="async"
                    draggable={false}
                    className="w-full h-28 object-cover transition-transform group-hover:scale-105"
                    onLoad={event => {
                      const img = event.currentTarget
                      setSizes(prev => ({
                        ...prev,
                        [url]: {
                          width: img.naturalWidth,
                          height: img.naturalHeight,
                        },
                      }))
                    }}
                  />
                  <div className="absolute bottom-1 left-1 flex gap-1">
                    {size && (
                      <Badge
                        variant={belowSpec ? 'destructive' : 'secondary'}
                        className="text-[10px] px-1.5 py-0"
                      >
                        {size.width}x{size.height}
                        {belowSpec ? ' below cover spec' : ''}
                      </Badge>
                    )}
                  </div>
                </button>
              )
            })}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
