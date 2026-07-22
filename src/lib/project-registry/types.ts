/**
 * Project Registry Types
 *
 * Simple type definitions for project identification and persistence
 */

/**
 * Utility type for deep partial - makes all nested properties optional
 * Used for settings updates where only changed fields need to be passed
 */
export type DeepPartial<T> = {
  [P in keyof T]?: T[P] extends object ? DeepPartial<T[P]> : T[P]
}

export interface ProjectMetadata {
  id: string // Generated project ID (package.json name + path hash if needed)
  name: string // From package.json
  path: string // Current full path
  lastOpened: string // ISO timestamp
  created: string // ISO timestamp
}

export interface ProjectSettings {
  // Project-specific overrides for paths
  pathOverrides: {
    contentDirectory?: string
    assetsDirectory?: string
    mdxComponentsDirectory?: string
  }
  // Project-specific overrides for frontmatter field mappings
  frontmatterMappings: {
    publishedDate?: string
    title?: string
    description?: string
    draft?: string
  }
  // Default file type for new files
  defaultFileType?: 'md' | 'mdx'
  // Override to use absolute paths for images (defaults to relative paths, matching Astro conventions)
  useAbsoluteAssetPaths?: boolean
  // Shell command run for images dropped into the editor instead of copying
  // them into the assets directory. Runs from the project root with the
  // image path appended as one argument; its stdout is inserted at the
  // cursor (e.g. a markdown snippet pointing at a CDN URL).
  imageDropCommand?: string
  // Directory (relative to project root) where cover images picked from a
  // post's remote images get downloaded (default: assets dir + collection)
  coverImagesDirectory?: string
  // Shell command for the Pull action (default: git pull --ff-only)
  pullCommand?: string
  // Publish pipeline (all run from the project root, see src/lib/publish.ts):
  // preflight prints the change set and a `changeset digest: <token>` line
  publishPreflightCommand?: string
  // optional long-running review server, stopped after the decision
  publishReviewCommand?: string
  // runs on approval; `{digest}` is replaced with the preflight token
  publishConfirmCommand?: string
  // One-click publish: skip the confirmation dialog and review server and
  // run the confirm command immediately after a successful preflight, with
  // progress in a toast. Failures still open the error dialog.
  publishAutoConfirm?: boolean
  // Collection-specific settings overrides
  collections?: CollectionSettings[]
}

// Collection-specific settings (subset of ProjectSettings)
export interface CollectionSpecificSettings {
  pathOverrides?: {
    contentDirectory?: string
    assetsDirectory?: string
  }
  frontmatterMappings?: {
    publishedDate?: string | string[]
    title?: string
    description?: string
    draft?: string
  }
  // Default file type for new files in this collection
  defaultFileType?: 'md' | 'mdx'
  // Override to use absolute paths for images (collection-level override)
  useAbsoluteAssetPaths?: boolean
  // Shell command for dropped images (collection-level override)
  imageDropCommand?: string
  // URL pattern template for content links (e.g. "/writing/{slug}")
  urlPattern?: string
}

export interface CollectionSettings {
  name: string // Collection identifier
  settings: CollectionSpecificSettings
}

export interface ProjectData {
  settings: ProjectSettings
  collections?: CollectionSettings[] // Collection-specific overrides
  version: number
}

export interface ProjectRegistry {
  projects: Record<string, ProjectMetadata> // projectId -> metadata
  lastOpenedProject: string | null
  version: number
}

export interface GlobalSettings {
  general: {
    ideCommand: string
    theme: 'light' | 'dark' | 'system'
    highlights: {
      nouns: boolean
      verbs: boolean
      adjectives: boolean
      adverbs: boolean
      conjunctions: boolean
    }
    autoSaveDelay: number
    defaultFileType: 'md' | 'mdx'
  }
  appearance: {
    headingColor: {
      light: string
      dark: string
    }
    editorBaseFontSize?: number // 1-30, default 18
  }
  version: number
}
