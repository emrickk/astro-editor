import { useState } from 'react'
import { ask } from '@tauri-apps/plugin-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../../ui/dialog'
import { Badge } from '../../ui/badge'
import {
  extractImageUrls,
  isDownloadableImageUrl,
  meetsCoverSpec,
} from '../../../lib/images'

interface PostImagePickerDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  editorContent: string
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
  editorContent,
  onSelect,
}: PostImagePickerDialogProps) {
  const [sizes, setSizes] = useState<
    Record<string, { width: number; height: number }>
  >({})

  const urls = extractImageUrls(editorContent).filter(isDownloadableImageUrl)

  const handleSelect = async (
    url: string,
    size: { width: number; height: number }
  ) => {
    if (!meetsCoverSpec(size.width, size.height)) {
      const confirmed = await ask(
        `This image is ${size.width}x${size.height}, below the recommended cover size. It may look blurry on large cards. Use it anyway?`,
        { title: 'Image Below Cover Spec', kind: 'warning' }
      )
      if (!confirmed) return
    }
    onSelect(url)
  }

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
            This post has no downloadable HTTPS images yet.
          </p>
        ) : (
          <div className="grid grid-cols-3 gap-3 max-h-[420px] overflow-y-auto pr-1">
            {urls.map(url => {
              const size = sizes[url]
              const belowSpec = size && !meetsCoverSpec(size.width, size.height)
              return (
                <button
                  key={url}
                  type="button"
                  onClick={() => size && void handleSelect(url, size)}
                  disabled={!size}
                  className="group relative rounded-md border overflow-hidden text-left focus:outline-none focus:ring-2 focus:ring-ring disabled:cursor-wait disabled:opacity-70"
                  title={size ? url : 'Loading image dimensions'}
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
                    onError={() => {
                      setSizes(prev => ({
                        ...prev,
                        [url]: { width: 0, height: 0 },
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
