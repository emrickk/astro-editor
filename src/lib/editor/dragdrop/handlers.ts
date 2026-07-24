import { EditorView } from '@codemirror/view'
import { useEditorStore } from '../../../store/editorStore'
import { useProjectStore } from '../../../store/projectStore'
import {
  tryAcquireProjectOperation,
  type ProjectOperationLease,
} from '../../../store/projectOperationLease'
import { isSameFileIdentity } from '../../images'
import { processDroppedFiles } from './fileProcessing'
import { validateDropContext, buildFallbackMarkdownForPaths } from './edgeCases'
import { FileDropPayload, DropResult } from './types'
import type { FileEntry } from '@/types'

function dropTargetIsCurrent(
  projectPath: string,
  currentFile: FileEntry,
  lease: ProjectOperationLease
): boolean {
  const project = useProjectStore.getState()
  const editor = useEditorStore.getState()
  return (
    lease.isCurrent() &&
    !project.isOperationLocked &&
    !editor.isOperationLocked &&
    project.projectPath === projectPath &&
    isSameFileIdentity(currentFile, editor.currentFile)
  )
}

/**
 * Hands the workflow mutex directly to one final synchronous editor mutation.
 * JavaScript cannot run another event handler between release and dispatch.
 */
function releaseForDropMutation(
  projectPath: string,
  currentFile: FileEntry,
  lease: ProjectOperationLease
): boolean {
  if (!dropTargetIsCurrent(projectPath, currentFile, lease)) return false
  lease.release()

  const project = useProjectStore.getState()
  const editor = useEditorStore.getState()
  return (
    !project.isOperationLocked &&
    !editor.isOperationLocked &&
    project.projectPath === projectPath &&
    isSameFileIdentity(currentFile, editor.currentFile)
  )
}

/**
 * Parse file drop payload from Tauri event
 * @param payload - Unknown payload from Tauri drag-drop event
 * @returns FileDropPayload with paths and position
 */
export const parseFileDropPayload = (
  payload: unknown
): { paths: string[]; position?: { x: number; y: number } } => {
  let filePaths: string[]
  let position: { x: number; y: number } | undefined

  if (Array.isArray(payload)) {
    filePaths = payload as string[]
  } else if (typeof payload === 'string') {
    filePaths = [payload]
  } else if (payload && typeof payload === 'object' && 'paths' in payload) {
    const payloadObj = payload as FileDropPayload
    filePaths = payloadObj.paths || []
    position = payloadObj.position
  } else {
    // eslint-disable-next-line no-console
    console.error('Unexpected payload format:', payload)
    return { paths: [] }
  }

  return { paths: filePaths, position }
}

/**
 * Check if drop position is within element bounds
 * @param position - Drop position
 * @param element - DOM element to check
 * @returns true if position is within element bounds
 */
export const isDropWithinElement = (
  position: { x: number; y: number } | undefined,
  element: Element | null
): boolean => {
  if (!position || !element) {
    return false
  }

  const rect = element.getBoundingClientRect()
  return (
    position.x >= rect.left &&
    position.x <= rect.right &&
    position.y >= rect.top &&
    position.y <= rect.bottom
  )
}

/**
 * Handle Tauri file drop events
 * @param payload - Payload from Tauri drag-drop event
 * @param editorView - CodeMirror editor view
 * @returns Promise that resolves when drop is handled
 */
export const handleTauriFileDrop = async (
  payload: unknown,
  editorView: EditorView | null
): Promise<DropResult> => {
  if (!editorView) {
    return { success: false, insertText: '', error: 'No editor view available' }
  }

  // Parse file paths and position from payload
  const { paths: filePaths, position } = parseFileDropPayload(payload)

  if (filePaths.length === 0) {
    return { success: false, insertText: '', error: 'No files in drop payload' }
  }

  // Check if drop is within editor element bounds
  // This prevents conflicts with FileUploadButton and other UI elements
  const editorElement = editorView.dom.closest('[data-editor-container]')

  const isWithin = isDropWithinElement(position, editorElement)

  if (!isWithin) {
    // Drop is outside editor bounds, ignore it
    return {
      success: false,
      insertText: '',
      error: 'Drop outside editor bounds',
    }
  }

  // Get current project path and file from store
  const { projectPath } = useProjectStore.getState()
  const { currentFile } = useEditorStore.getState()

  // Validate context and handle edge cases
  const validation = validateDropContext(projectPath, currentFile)

  if (!validation.canProceed) {
    const projectLocked = useProjectStore.getState().isOperationLocked
    const editorLocked = useEditorStore.getState().isOperationLocked
    if (projectLocked || editorLocked) {
      return {
        success: false,
        insertText: '',
        error: 'Finish the current pull or publish before dropping files',
      }
    }

    const fallbackText = buildFallbackMarkdownForPaths(filePaths)

    // Insert fallback text
    const { state } = editorView
    const { from } = state.selection.main

    editorView.dispatch({
      changes: {
        from: from,
        to: from,
        insert: fallbackText,
      },
    })

    return {
      success: false,
      insertText: fallbackText,
      error: validation.reason,
    }
  }

  const targetFile = currentFile!
  const lease = tryAcquireProjectOperation('image', projectPath!)
  if (!lease) {
    return {
      success: false,
      insertText: '',
      error: 'Finish the current project operation before dropping files',
    }
  }

  // Process files normally
  try {
    const processedFiles = await processDroppedFiles(
      filePaths,
      projectPath!,
      targetFile.collection
    )

    const insertText = processedFiles
      .map(file => file.markdownText)
      .filter(text => text.length > 0)
      .join('\n')

    if (!releaseForDropMutation(projectPath!, targetFile, lease)) {
      return {
        success: false,
        insertText: '',
        error:
          'The project or open file changed while the dropped files were being processed',
      }
    }

    // Insert processed text at cursor position
    const { state } = editorView
    const { from } = state.selection.main

    editorView.dispatch({
      changes: { from, insert: insertText },
      selection: { anchor: from + insertText.length },
    })

    return { success: true, insertText }
  } catch {
    // Handle processing errors
    const fallbackText = buildFallbackMarkdownForPaths(filePaths)

    if (!releaseForDropMutation(projectPath!, targetFile, lease)) {
      return {
        success: false,
        insertText: '',
        error:
          'The project or open file changed while the dropped files were being processed',
      }
    }

    const { state } = editorView
    const { from } = state.selection.main

    editorView.dispatch({
      changes: {
        from: from,
        to: from,
        insert: fallbackText,
      },
    })

    return {
      success: false,
      insertText: fallbackText,
      error: 'Processing failed',
    }
  } finally {
    lease.release()
  }
}
