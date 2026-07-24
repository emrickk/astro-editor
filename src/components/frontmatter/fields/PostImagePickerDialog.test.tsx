import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ask } from '@tauri-apps/plugin-dialog'
import { PostImagePickerDialog } from './PostImagePickerDialog'

vi.mock('@tauri-apps/plugin-dialog', () => ({ ask: vi.fn() }))

const HTTPS_IMAGE = 'https://cdn.example.com/2026/07/photo.webp'

function loadImage(width: number, height: number) {
  const image = screen.getByRole('img')
  Object.defineProperties(image, {
    naturalWidth: { configurable: true, value: width },
    naturalHeight: { configurable: true, value: height },
  })
  fireEvent.load(image)
}

describe('PostImagePickerDialog', () => {
  beforeEach(() => {
    vi.mocked(ask).mockReset()
  })

  it('shows only HTTPS images supported by the download backend', () => {
    render(
      <PostImagePickerDialog
        open
        onOpenChange={vi.fn()}
        editorContent={`![secure](${HTTPS_IMAGE})\n![http](http://example.com/x.webp)\n![inline](data:image/png;base64,abc)`}
        onSelect={vi.fn()}
      />
    )

    expect(screen.getAllByRole('img')).toHaveLength(1)
    expect(screen.getByRole('img')).toHaveAttribute('src', HTTPS_IMAGE)
  })

  it('requires confirmation before selecting a below-spec cover', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn()
    vi.mocked(ask).mockResolvedValue(false)
    render(
      <PostImagePickerDialog
        open
        onOpenChange={vi.fn()}
        editorContent={`![small](${HTTPS_IMAGE})`}
        onSelect={onSelect}
      />
    )
    loadImage(2000, 1000)

    await user.click(screen.getByTitle(HTTPS_IMAGE))

    expect(ask).toHaveBeenCalledWith(
      expect.stringContaining('2000x1000'),
      expect.objectContaining({ kind: 'warning' })
    )
    expect(onSelect).not.toHaveBeenCalled()
  })

  it('selects a spec-compliant cover without a warning', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn()
    render(
      <PostImagePickerDialog
        open
        onOpenChange={vi.fn()}
        editorContent={`![wide](${HTTPS_IMAGE})`}
        onSelect={onSelect}
      />
    )
    loadImage(2400, 1260)

    await user.click(screen.getByTitle(HTTPS_IMAGE))

    expect(ask).not.toHaveBeenCalled()
    expect(onSelect).toHaveBeenCalledWith(HTTPS_IMAGE)
  })
})
