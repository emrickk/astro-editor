/**
 * Bilingual sibling-file support.
 *
 * Blogs that pair posts as `<slug>.md` + `<slug>.zh.md` / `<slug>.en.md`
 * (linked by a `translationKey` frontmatter field) keep each language's
 * body in its own file. Given one file of a pair, derive the disk paths
 * where its sibling translation may live, in priority order.
 */

const SIBLING_LANGS = ['zh', 'en'] as const
const EXTENSIONS = ['md', 'mdx'] as const

const LANG_SIBLING_RE = /^(.+)\.(zh|en)\.(md|mdx)$/
const PRIMARY_RE = /^(.+)\.(md|mdx)$/

function splitPath(filePath: string): { dir: string; name: string } {
  const idx = filePath.lastIndexOf('/')
  if (idx === -1) return { dir: '', name: filePath }
  return { dir: filePath.slice(0, idx + 1), name: filePath.slice(idx + 1) }
}

/** Orders extensions so the current file's extension is tried first. */
function orderedExtensions(currentExt: string): string[] {
  return [...EXTENSIONS].sort((a, b) =>
    a === currentExt ? -1 : b === currentExt ? 1 : 0
  )
}

/**
 * Candidate absolute paths for the sibling translation of `filePath`,
 * most likely first. Returns [] when the file is not a markdown file.
 *
 * - `dir/slug.zh.md` -> [`dir/slug.md`, `dir/slug.mdx`]
 * - `dir/slug.md`    -> [`dir/slug.zh.md`, `dir/slug.en.md`,
 *                        `dir/slug.zh.mdx`, `dir/slug.en.mdx`]
 */
export function getSiblingCandidatePaths(filePath: string): string[] {
  const { dir, name } = splitPath(filePath)

  const langMatch = name.match(LANG_SIBLING_RE)
  if (langMatch) {
    const [, base, , ext] = langMatch
    return orderedExtensions(ext!).map(e => `${dir}${base}.${e}`)
  }

  const primaryMatch = name.match(PRIMARY_RE)
  if (primaryMatch) {
    const [, base, ext] = primaryMatch
    return orderedExtensions(ext!).flatMap(e =>
      SIBLING_LANGS.map(lang => `${dir}${base}.${lang}.${e}`)
    )
  }

  return []
}
