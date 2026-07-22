import { useMemo } from 'react'
import { useEditorStore } from '../store/editorStore'
import { useProjectStore } from '../store/projectStore'
import { useCollectionsQuery } from './queries/useCollectionsQuery'
import { useDirectoryScanQuery } from './queries/useDirectoryScanQuery'

const MAX_DISTINCT = 30
const MAX_VALUE_LENGTH = 60
const MIN_FILES = 4

/**
 * Existing values of a frontmatter field across the open file's collection,
 * for fields that behave like a closed category set (few distinct values,
 * heavily reused). Free-form fields (titles, keys) return [] so their inputs
 * stay plain. Backed by the same directory scan the sidebar already caches.
 */
export function useFieldSuggestions(fieldName: string): string[] {
  const projectPath = useProjectStore(state => state.projectPath)
  const currentProjectSettings = useProjectStore(
    state => state.currentProjectSettings
  )
  const collectionName = useEditorStore(
    state => state.currentFile?.collection ?? null
  )
  const { data: collections = [] } = useCollectionsQuery(
    projectPath,
    currentProjectSettings
  )
  const collectionPath =
    collections.find(c => c.name === collectionName)?.path ?? null
  const { data: dirContents } = useDirectoryScanQuery(
    projectPath,
    collectionName,
    collectionPath,
    null
  )

  return useMemo(() => {
    const files = dirContents?.files ?? []
    if (files.length < MIN_FILES) return []
    const values = files
      .map(file => file.frontmatter?.[fieldName])
      .filter(
        (value): value is string =>
          typeof value === 'string' &&
          value.trim() !== '' &&
          value.length <= MAX_VALUE_LENGTH
      )
    if (values.length === 0) return []
    const distinct = [...new Set(values)]
    // Categorical shape: a small set of values that repeat across files
    if (distinct.length < 2 || distinct.length > MAX_DISTINCT) return []
    if (distinct.length / values.length > 0.5) return []
    return distinct.sort((a, b) => a.localeCompare(b))
  }, [dirContents, fieldName])
}
