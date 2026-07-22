import { Menu, MenuItem, PredefinedMenuItem } from '@tauri-apps/api/menu'
import { LogicalPosition } from '@tauri-apps/api/dpi'
import { commands } from '@/lib/bindings'
import { remove } from '@tauri-apps/plugin-fs'
import { openPath } from '@tauri-apps/plugin-opener'
import { ask } from '@tauri-apps/plugin-dialog'
import type { FileEntry } from '@/types'
import { useProjectStore } from '../../store/projectStore'
import { useEditorStore } from '../../store/editorStore'
import { usePublishStore } from '../../store/publishStore'
import { openInIde } from '../../lib/ide'
import { getTitle } from '@/lib/files/sorting'
import { getSiblingCandidatePaths } from '../../lib/translations'
import {
  getEffectiveContentDirectory,
} from '../../lib/project-registry'
import { ASTRO_PATHS } from '../../lib/constants'
import { getPlatform } from '@/hooks/usePlatform'
import { getPlatformString } from '@/lib/platform-strings'

interface ContextMenuOptions {
  file: FileEntry
  position: { x: number; y: number }
  onRefresh?: () => void
  onRename?: (file: FileEntry) => void
}

export class FileContextMenu {
  private static async showConfirmationDialog(
    fileName: string,
    withSibling: boolean
  ): Promise<boolean> {
    return ask(
      withSibling
        ? `Delete "${fileName}" and its translation file? Both language files will be removed.`
        : `Are you sure you want to delete "${fileName}"?`,
      {
        title: 'Delete Post',
        kind: 'warning',
      }
    )
  }

  /** Existing sibling translation file for a post, resolved on disk. */
  private static async findSiblingPath(
    filePath: string,
    projectPath: string
  ): Promise<string | null> {
    const { currentProjectSettings } = useProjectStore.getState()
    const contentDirectory = getEffectiveContentDirectory(
      currentProjectSettings
    )
    for (const candidate of getSiblingCandidatePaths(filePath)) {
      const result = await commands.resolveFileEntry(
        candidate,
        projectPath,
        contentDirectory !== ASTRO_PATHS.CONTENT_DIR ? contentDirectory : null
      )
      if (result.status === 'ok' && result.data) {
        return result.data.path
      }
    }
    return null
  }

  /**
   * Deletes a post (and its sibling translation when one exists), closes it
   * in the editor if open, and offers to publish the removal so the site
   * stops serving the post.
   */
  private static async deletePost(
    file: FileEntry,
    fileName: string,
    projectPath: string,
    onRefresh?: () => void
  ): Promise<void> {
    const siblingPath = await FileContextMenu.findSiblingPath(
      file.path,
      projectPath
    )
    const confirmed = await FileContextMenu.showConfirmationDialog(
      fileName,
      siblingPath !== null
    )
    if (!confirmed) return

    await remove(file.path)
    if (siblingPath) {
      await remove(siblingPath)
    }

    const { currentFile, closeCurrentFile } = useEditorStore.getState()
    if (
      currentFile &&
      (currentFile.path === file.path || currentFile.path === siblingPath)
    ) {
      closeCurrentFile()
    }
    if (onRefresh) {
      onRefresh()
    }

    // A post that was ever published stays on the site until the removal
    // ships. Offer it right here; an unpublished draft just reports
    // "nothing to publish".
    const { currentProjectSettings } = useProjectStore.getState()
    if (
      currentProjectSettings?.publishPreflightCommand?.trim() &&
      currentProjectSettings?.publishConfirmCommand?.trim()
    ) {
      const publishRemoval = await ask(
        'The site keeps showing this post until the removal is published. Publish the removal now?',
        { title: 'Publish Removal', kind: 'info' }
      )
      if (publishRemoval) {
        const prefix = projectPath.endsWith('/')
          ? projectPath
          : `${projectPath}/`
        const relPaths = [file.path, siblingPath]
          .filter((p): p is string => p !== null)
          .map(p => (p.startsWith(prefix) ? p.slice(prefix.length) : p))
        void usePublishStore.getState().startPublish(relPaths)
      }
    }
  }

  private static getIdeCommand(): string | null {
    try {
      // Access global settings directly from the store
      const { globalSettings } = useProjectStore.getState()
      return globalSettings?.general?.ideCommand || null
    } catch {
      return null
    }
  }

  private static generateDuplicatePath(originalPath: string): string {
    const lastSlashIndex = originalPath.lastIndexOf('/')
    const directory = originalPath.substring(0, lastSlashIndex)
    const fileName = originalPath.substring(lastSlashIndex + 1)

    const lastDotIndex = fileName.lastIndexOf('.')
    if (lastDotIndex === -1) {
      // No extension
      return `${directory}/${fileName}-1`
    }

    const nameWithoutExt = fileName.substring(0, lastDotIndex)
    const extension = fileName.substring(lastDotIndex)
    return `${directory}/${nameWithoutExt}-1${extension}`
  }

  static async show({
    file,
    position,
    onRefresh,
    onRename,
  }: ContextMenuOptions): Promise<void> {
    try {
      // Get current IDE setting from global preferences
      const ideCommand = FileContextMenu.getIdeCommand()

      // Get project path and settings from store
      const { projectPath, currentProjectSettings } = useProjectStore.getState()
      if (!projectPath) {
        throw new Error('No project path available')
      }

      // Get title field from settings (defaults to 'title')
      const titleField =
        currentProjectSettings?.frontmatterMappings?.title || 'title'

      // Create menu items
      const currentPlatform = getPlatform()
      const revealItem = await MenuItem.new({
        id: 'reveal-in-finder',
        text: getPlatformString('revealInFileManager', currentPlatform),
        action: () => {
          void (async () => {
            try {
              // Get the directory containing the file
              const directory = file.path.substring(
                0,
                file.path.lastIndexOf('/')
              )
              await openPath(directory)
            } catch (error) {
              // eslint-disable-next-line no-console
              console.error('Failed to reveal in file manager:', error)
            }
          })()
        },
      })

      const copyPathItem = await MenuItem.new({
        id: 'copy-path',
        text: 'Copy Path',
        action: () => {
          void (async () => {
            try {
              const result = await commands.copyTextToClipboard(file.path)
              if (result.status === 'error') {
                throw new Error(result.error)
              }
            } catch (error) {
              // eslint-disable-next-line no-console
              console.error('Failed to copy path:', error)
            }
          })()
        },
      })

      const duplicateItem = await MenuItem.new({
        id: 'duplicate-file',
        text: 'Duplicate',
        action: () => {
          void (async () => {
            try {
              const duplicatePath = FileContextMenu.generateDuplicatePath(
                file.path
              )

              // Read the original file content
              const readResult = await commands.readFile(file.path, projectPath)
              if (readResult.status === 'error') {
                throw new Error(readResult.error)
              }

              // Parse the duplicate path into directory and filename
              const lastSlashIndex = duplicatePath.lastIndexOf('/')
              const directory = duplicatePath.substring(0, lastSlashIndex)
              const filename = duplicatePath.substring(lastSlashIndex + 1)

              // Create the duplicate file
              const createResult = await commands.createFile(
                directory,
                filename,
                readResult.data,
                projectPath
              )
              if (createResult.status === 'error') {
                throw new Error(createResult.error)
              }

              // Refresh the file list if callback is provided
              if (onRefresh) {
                onRefresh()
              }
            } catch (error) {
              // eslint-disable-next-line no-console
              console.error('Failed to duplicate file:', error)
            }
          })()
        },
      })

      const renameItem = await MenuItem.new({
        id: 'rename-file',
        text: 'Rename',
        action: () => {
          try {
            if (onRename) {
              onRename(file)
            }
          } catch (error) {
            // eslint-disable-next-line no-console
            console.error('Failed to initiate rename:', error)
          }
        },
      })

      // Create "Open in IDE" menu item if IDE is configured
      const openInIdeItem = ideCommand
        ? await MenuItem.new({
            id: 'open-in-ide',
            text: 'Open in IDE',
            action: () => {
              void openInIde(file.path, ideCommand)
            },
          })
        : null

      const separator = await PredefinedMenuItem.new({
        text: 'separator',
        item: 'Separator',
      })

      const deleteItem = await MenuItem.new({
        id: 'delete-file',
        text: 'Delete',
        action: () => {
          void (async () => {
            try {
              const fileName = getTitle(file, titleField)
              await FileContextMenu.deletePost(
                file,
                fileName,
                projectPath,
                onRefresh
              )
            } catch (error) {
              // eslint-disable-next-line no-console
              console.error('Failed to delete file:', error)
            }
          })()
        },
      })

      // Create and show the context menu
      const menuItems = [
        revealItem,
        copyPathItem,
        duplicateItem,
        renameItem,
        ...(openInIdeItem ? [openInIdeItem] : []),
        separator,
        deleteItem,
      ]

      const menu = await Menu.new({
        items: menuItems,
      })

      // Show the menu at the specified position
      await menu.popup(new LogicalPosition(position.x, position.y))
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error('Failed to show context menu:', error)
    }
  }
}
