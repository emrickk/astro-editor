import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { exists } from '@tauri-apps/plugin-fs'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { commands } from '@/types'
import { processFileToAssets } from '../../../lib/files'
import { useEditorStore } from '../../../store/editorStore'
import { useProjectStore } from '../../../store/projectStore'
import {
  getActiveProjectOperation,
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from '../../../store/projectOperationLease'
import { usePublishStore } from '../../../store/publishStore'
import { ImageField } from './ImageField'
import type { FileEntry } from '@/types'
import type { ReactNode } from 'react'

vi.mock('@tauri-apps/plugin-fs', () => ({ exists: vi.fn() }))

vi.mock('../../../lib/files', async importOriginal => {
  const actual = await importOriginal<typeof import('../../../lib/files')>()
  return { ...actual, processFileToAssets: vi.fn() }
})

vi.mock('../../tauri', () => ({
  FileUploadButton: ({
    onFileSelect,
    disabled,
    children,
  }: {
    onFileSelect: (path: string) => void | Promise<void>
    disabled?: boolean
    children: ReactNode
  }) => (
    <button
      type="button"
      data-testid="file-select"
      disabled={disabled}
      onClick={() => void onFileSelect('/tmp/cover.png')}
    >
      {children}
    </button>
  ),
}))

vi.mock('./FieldWrapper', () => ({
  FieldWrapper: ({ children }: { children: ReactNode }) => (
    <div>{children}</div>
  ),
}))

vi.mock('./ImageThumbnail', () => ({ ImageThumbnail: () => null }))

const REMOTE_IMAGE = 'https://cdn.example.com/2026/07/cover.webp'

vi.mock('./PostImagePickerDialog', () => ({
  PostImagePickerDialog: ({
    open,
    onSelect,
  }: {
    open: boolean
    onSelect: (url: string) => void
  }) =>
    open ? (
      <button
        type="button"
        onClick={() => onSelect('https://cdn.example.com/2026/07/cover.webp')}
      >
        Choose remote image
      </button>
    ) : null,
}))

const PROJECT_PATH = '/project'
const CURRENT_FILE: FileEntry = {
  id: 'posts/current',
  path: '/project/src/content/posts/current.md',
  name: 'current',
  extension: 'md',
  collection: 'posts',
  last_modified: null,
  frontmatter: null,
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(resolvePromise => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}

describe('ImageField operation safety', () => {
  const updateFrontmatterField = vi.fn()
  const downloadImage = vi.spyOn(commands, 'downloadImageToProject')

  beforeEach(() => {
    vi.clearAllMocks()
    resetProjectOperationLeaseForTests()
    usePublishStore.setState({ stage: 'idle' })
    useProjectStore.setState({
      projectPath: PROJECT_PATH,
      currentProjectSettings: null,
      isOperationLocked: false,
    })
    useEditorStore.setState({
      currentFile: CURRENT_FILE,
      editorContent: `![cover](${REMOTE_IMAGE})`,
      frontmatter: {},
      isOperationLocked: false,
      updateFrontmatterField,
    })
    vi.mocked(exists).mockResolvedValue(false)
    downloadImage.mockResolvedValue({
      status: 'ok',
      data: '/project/src/assets/hero/2026/07/cover.webp',
    })
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('claims the workflow lease before awaiting a selected file', async () => {
    const user = userEvent.setup()
    const processing = deferred<{
      relativePath: string
      wasCopied: boolean
      filename: string
    }>()
    vi.mocked(processFileToAssets).mockReturnValue(processing.promise)
    render(<ImageField name="heroImage" label="Cover" />)

    await user.click(screen.getByTestId('file-select'))

    expect(getActiveProjectOperation()).toEqual({
      kind: 'image',
      projectPath: PROJECT_PATH,
    })
    expect(tryAcquireProjectOperation('publish', PROJECT_PATH)).toBeNull()

    await act(async () => {
      processing.resolve({
        relativePath: '../../assets/hero/2026/07/cover.webp',
        wasCopied: true,
        filename: 'cover.webp',
      })
      await processing.promise
    })

    await waitFor(() =>
      expect(updateFrontmatterField).toHaveBeenCalledWith(
        'heroImage',
        '../../assets/hero/2026/07/cover.webp'
      )
    )
    expect(getActiveProjectOperation()).toBeNull()
  })

  it('does not apply a selected image after the open file changes', async () => {
    const user = userEvent.setup()
    const processing = deferred<{
      relativePath: string
      wasCopied: boolean
      filename: string
    }>()
    vi.mocked(processFileToAssets).mockReturnValue(processing.promise)
    render(<ImageField name="heroImage" label="Cover" />)
    await user.click(screen.getByTestId('file-select'))

    useEditorStore.setState({
      currentFile: { ...CURRENT_FILE, id: 'posts/other', name: 'other' },
    })
    await act(async () => {
      processing.resolve({
        relativePath: '../../assets/hero/2026/07/cover.webp',
        wasCopied: true,
        filename: 'cover.webp',
      })
      await processing.promise
    })

    await waitFor(() => expect(getActiveProjectOperation()).toBeNull())
    expect(updateFrontmatterField).not.toHaveBeenCalled()
  })

  it.each([
    ['manual cover edit', { heroImage: '/manual/new-cover.webp' }],
    ['cover clear', {}],
  ])('keeps a newer %s while image processing is pending', async (_, next) => {
    const user = userEvent.setup()
    const processing = deferred<{
      relativePath: string
      wasCopied: boolean
      filename: string
    }>()
    useEditorStore.setState({
      frontmatter: { heroImage: '/original/cover.webp' },
    })
    vi.mocked(processFileToAssets).mockReturnValue(processing.promise)
    render(<ImageField name="heroImage" label="Cover" />)
    await user.click(screen.getByTestId('file-select'))

    useEditorStore.setState({ frontmatter: next })
    await act(async () => {
      processing.resolve({
        relativePath: '../../assets/hero/2026/07/slow-cover.webp',
        wasCopied: true,
        filename: 'slow-cover.webp',
      })
      await processing.promise
    })

    await waitFor(() => expect(getActiveProjectOperation()).toBeNull())
    expect(useEditorStore.getState().frontmatter).toEqual(next)
    expect(updateFrontmatterField).not.toHaveBeenCalled()
  })

  it('claims the workflow lease before resolving a remote cover destination', async () => {
    const user = userEvent.setup()
    const pathCheck = deferred<boolean>()
    vi.mocked(exists).mockReturnValue(pathCheck.promise)
    vi.mocked(processFileToAssets).mockResolvedValue({
      relativePath: '../../assets/hero/2026/07/cover.webp',
      wasCopied: false,
      filename: 'cover.webp',
    })
    render(<ImageField name="heroImage" label="Cover" />)

    await user.click(screen.getByRole('button', { name: /from post/i }))
    await user.click(
      screen.getByRole('button', { name: 'Choose remote image' })
    )

    expect(getActiveProjectOperation()).toEqual({
      kind: 'image',
      projectPath: PROJECT_PATH,
    })
    expect(tryAcquireProjectOperation('pull', PROJECT_PATH)).toBeNull()
    expect(downloadImage).not.toHaveBeenCalled()

    await act(async () => {
      pathCheck.resolve(false)
      await pathCheck.promise
    })

    await waitFor(() => expect(downloadImage).toHaveBeenCalledTimes(1))
    await waitFor(() =>
      expect(updateFrontmatterField).toHaveBeenCalledWith(
        'heroImage',
        '../../assets/hero/2026/07/cover.webp'
      )
    )
    expect(getActiveProjectOperation()).toBeNull()
  })
})
