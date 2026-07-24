import React, { useState } from 'react'
import { exists } from '@tauri-apps/plugin-fs'
import { useEditorStore } from '../../../store/editorStore'
import { getNestedValue } from '../../../lib/object-utils'
import { useProjectStore } from '../../../store/projectStore'
import {
  tryAcquireProjectOperation,
  type ProjectOperationLease,
} from '../../../store/projectOperationLease'
import {
  isProjectActionLocked,
  usePublishStore,
} from '../../../store/publishStore'
import { FieldWrapper } from './FieldWrapper'
import { ImageThumbnail } from './ImageThumbnail'
import { FileUploadButton } from '../../tauri'
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
} from '../../ui/input-group'
import { processFileToAssets, IMAGE_EXTENSIONS } from '../../../lib/files'
import {
  getCollectionSettings,
  type ProjectSettings,
} from '../../../lib/project-registry'
import {
  coverDestinationFor,
  findAvailableCoverDestination,
  isSameFileIdentity,
} from '../../../lib/images'
import { commands } from '@/types'
import { Button } from '../../ui/button'
import { PostImagePickerDialog } from './PostImagePickerDialog'
import { X, Loader2, Edit3, Check, Images } from 'lucide-react'
import type { FieldProps } from '../../../types/common'
import type { SchemaField } from '../../../lib/schema'
import type { FileEntry } from '@/types'

interface ImageFieldProps extends FieldProps {
  field?: SchemaField
}

interface ImageTargetContext {
  projectPath: string
  currentProjectSettings: ProjectSettings | null
  currentFile: FileEntry
  frontmatterRevision: Record<string, unknown>
  fieldValue: unknown
}

interface PickerSource {
  target: ImageTargetContext
  editorContent: string
}

function absoluteProjectPath(
  projectPath: string,
  relativePath: string
): string {
  return `${projectPath.replace(/[\\/]+$/, '')}/${relativePath.replace(/^[\\/]+/, '')}`
}

export const ImageField: React.FC<ImageFieldProps> = ({
  name,
  label,
  required,
  field,
}) => {
  const value = useEditorStore(state => getNestedValue(state.frontmatter, name))
  const updateFrontmatterField = useEditorStore(
    state => state.updateFrontmatterField
  )
  const [isLoading, setIsLoading] = useState(false)
  const [isEditing, setIsEditing] = useState(false)
  const [editValue, setEditValue] = useState('')
  const [pickerOpen, setPickerOpen] = useState(false)
  const [pickerSource, setPickerSource] = useState<PickerSource | null>(null)
  const projectActionLocked = usePublishStore(state =>
    isProjectActionLocked(state.stage)
  )

  const stringValue = typeof value === 'string' ? value : ''
  // When editing, show edit value; otherwise show current value
  const displayValue = isEditing ? editValue : stringValue

  const captureTarget = (): ImageTargetContext | null => {
    const { projectPath, currentProjectSettings } = useProjectStore.getState()
    const { currentFile, frontmatter } = useEditorStore.getState()
    if (!projectPath || !currentFile || !currentFile.collection) return null
    return {
      projectPath,
      currentProjectSettings,
      currentFile,
      frontmatterRevision: frontmatter,
      fieldValue: getNestedValue(frontmatter, name),
    }
  }

  const targetIsCurrent = (target: ImageTargetContext): boolean => {
    const { projectPath } = useProjectStore.getState()
    const { currentFile, frontmatter } = useEditorStore.getState()
    return (
      projectPath === target.projectPath &&
      isSameFileIdentity(target.currentFile, currentFile) &&
      frontmatter === target.frontmatterRevision &&
      Object.is(getNestedValue(frontmatter, name), target.fieldValue)
    )
  }

  const assertTargetCanMutate = (
    target: ImageTargetContext,
    lease: ProjectOperationLease
  ) => {
    const project = useProjectStore.getState()
    const editor = useEditorStore.getState()
    if (!lease.isCurrent()) {
      throw new Error(
        'Another project operation started while the image was being processed.'
      )
    }
    if (project.isOperationLocked || editor.isOperationLocked) {
      throw new Error(
        'Wait for the current pull or publish operation to finish before changing a cover.'
      )
    }
    if (!targetIsCurrent(target)) {
      throw new Error(
        'The open post or cover changed while the image was being processed. The newer edit was kept.'
      )
    }
  }

  const releaseForFrontmatterMutation = (
    target: ImageTargetContext,
    lease: ProjectOperationLease
  ) => {
    assertTargetCanMutate(target, lease)
    lease.release()

    const project = useProjectStore.getState()
    const editor = useEditorStore.getState()
    if (
      project.isOperationLocked ||
      editor.isOperationLocked ||
      !targetIsCurrent(target)
    ) {
      throw new Error(
        'The project, open post, or cover changed before the image could be applied. The newer edit was kept.'
      )
    }
  }

  const processImageForTarget = async (
    filePath: string,
    target: ImageTargetContext,
    lease: ProjectOperationLease
  ) => {
    assertTargetCanMutate(target, lease)

    const effectiveSettings = getCollectionSettings(
      target.currentProjectSettings,
      target.currentFile.collection
    )
    const result = await processFileToAssets({
      sourcePath: filePath,
      projectPath: target.projectPath,
      collection: target.currentFile.collection,
      projectSettings: target.currentProjectSettings,
      copyStrategy: 'only-if-outside-project',
      currentFilePath: target.currentFile.path,
      useRelativePaths: effectiveSettings.useRelativeAssetPaths,
    })

    releaseForFrontmatterMutation(target, lease)
    updateFrontmatterField(name, result.relativePath)
  }

  const showImageError = (title: string, error: unknown) => {
    window.dispatchEvent(
      new CustomEvent('toast', {
        detail: {
          title,
          description: error instanceof Error ? error.message : 'Unknown error',
          variant: 'destructive',
        },
      })
    )
  }

  const handleFileSelect = async (filePath: string) => {
    const target = captureTarget()
    if (!target) {
      showImageError(
        'Failed to add image',
        new Error('No project or collection context available')
      )
      return
    }

    const lease = tryAcquireProjectOperation('image', target.projectPath)
    if (!lease) {
      showImageError(
        'Failed to add image',
        new Error(
          'Finish the current pull, publish, or image operation before changing a cover.'
        )
      )
      return
    }

    setIsLoading(true)
    try {
      await processImageForTarget(filePath, target, lease)
    } catch (error) {
      showImageError('Failed to add image', error)
    } finally {
      lease.release()
      setIsLoading(false)
    }
  }

  /**
   * Cover from a post image: download the remote image into the project's
   * cover directory (yyyy/mm mirrors the CDN key), then run it through the
   * normal selection flow, which uses in-project files in place.
   */
  const handlePostImageSelect = async (url: string) => {
    setPickerOpen(false)
    const target = pickerSource?.target
    if (!target) return

    const lease = tryAcquireProjectOperation('image', target.projectPath)
    if (!lease) {
      showImageError(
        'Failed to use post image',
        new Error(
          'Finish the current pull, publish, or image operation before changing a cover.'
        )
      )
      return
    }

    setIsLoading(true)
    try {
      assertTargetCanMutate(target, lease)
      const effectiveSettings = getCollectionSettings(
        target.currentProjectSettings,
        target.currentFile.collection
      )
      const coverDir =
        target.currentProjectSettings?.coverImagesDirectory?.trim() ||
        `${effectiveSettings.pathOverrides.assetsDirectory.replace(/\/+$/, '')}/${target.currentFile.collection}`
      const preferredDest = coverDestinationFor(url, coverDir)
      const dest = await findAvailableCoverDestination(
        preferredDest,
        async candidate =>
          exists(absoluteProjectPath(target.projectPath, candidate))
      )

      assertTargetCanMutate(target, lease)
      const result = await commands.downloadImageToProject(
        url,
        dest,
        target.projectPath
      )
      if (result.status === 'error') {
        throw new Error(result.error)
      }
      await processImageForTarget(result.data, target, lease)
    } catch (error) {
      showImageError('Failed to use post image', error)
    } finally {
      lease.release()
      setIsLoading(false)
    }
  }

  const handlePickerOpen = () => {
    const target = captureTarget()
    if (!target) {
      showImageError(
        'Failed to open image picker',
        new Error('No project or collection context available')
      )
      return
    }
    setPickerSource({
      target,
      editorContent: useEditorStore.getState().editorContent,
    })
    setPickerOpen(true)
  }

  const handleClear = () => {
    updateFrontmatterField(name, undefined)
  }

  const handleEditStart = () => {
    setEditValue(stringValue)
    setIsEditing(true)
  }

  const handleEditCancel = () => {
    setEditValue('')
    setIsEditing(false)
  }

  const handleEditSave = () => {
    const trimmedPath = editValue.trim()

    // Empty path means clear
    if (!trimmedPath) {
      updateFrontmatterField(name, undefined)
      setIsEditing(false)
      setEditValue('')
      return
    }

    // Manual edit: user has full control, no validation
    // If the path doesn't exist, the preview will simply fail to load
    updateFrontmatterField(name, trimmedPath)
    setIsEditing(false)
    setEditValue('')
  }

  return (
    <FieldWrapper
      label={label}
      required={required}
      description={
        field && 'description' in field ? field.description : undefined
      }
      defaultValue={field?.default}
      constraints={field?.constraints}
      currentValue={value}
    >
      <div className="space-y-2">
        {/* Path display/edit with InputGroup - only shown when image exists */}
        {stringValue && (
          <InputGroup>
            <InputGroupInput
              type="text"
              value={displayValue}
              onChange={e => setEditValue(e.target.value)}
              placeholder="Enter image path (e.g., /src/assets/image.jpg)"
              disabled={!isEditing}
              onKeyDown={e => {
                if (!isEditing) return
                if (e.key === 'Enter') {
                  e.preventDefault()
                  handleEditSave()
                } else if (e.key === 'Escape') {
                  e.preventDefault()
                  handleEditCancel()
                }
              }}
            />
            <InputGroupAddon align="inline-end">
              {isEditing ? (
                <>
                  <InputGroupButton
                    size="icon-xs"
                    onClick={handleEditSave}
                    title="Save path"
                  >
                    <Check className="size-3.5" />
                  </InputGroupButton>
                  <InputGroupButton
                    size="icon-xs"
                    onClick={handleEditCancel}
                    title="Cancel"
                  >
                    <X className="size-3.5" />
                  </InputGroupButton>
                </>
              ) : (
                <>
                  <InputGroupButton
                    size="icon-xs"
                    onClick={handleEditStart}
                    title="Edit path manually"
                  >
                    <Edit3 className="size-3.5" />
                  </InputGroupButton>
                  <InputGroupButton
                    size="icon-xs"
                    onClick={handleClear}
                    title="Clear image"
                  >
                    <X className="size-3.5" />
                  </InputGroupButton>
                </>
              )}
            </InputGroupAddon>
          </InputGroup>
        )}

        {/* File upload + from-post buttons - above preview */}
        <div className="flex items-center gap-2">
          <FileUploadButton
            accept={[...IMAGE_EXTENSIONS]}
            onFileSelect={handleFileSelect}
            disabled={isLoading || projectActionLocked}
          >
            {isLoading && <Loader2 className="mr-2 size-4 animate-spin" />}
            {stringValue ? 'Change Image' : 'Select Image'}
          </FileUploadButton>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={isLoading || projectActionLocked}
            onClick={handlePickerOpen}
            title="Choose one of the images already in this post"
          >
            <Images className="mr-1.5 size-4" />
            From Post
          </Button>
        </div>
        <PostImagePickerDialog
          open={pickerOpen}
          onOpenChange={setPickerOpen}
          editorContent={pickerSource?.editorContent ?? ''}
          onSelect={url => void handlePostImageSelect(url)}
        />

        {/* Preview - always shown when stringValue exists, never hidden during editing */}
        {stringValue && <ImageThumbnail path={stringValue} />}
      </div>
    </FieldWrapper>
  )
}
