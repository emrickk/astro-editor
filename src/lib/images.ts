/**
 * Image references in post bodies: markdown images and raw <img> tags.
 * Shared by the editor's inline preview and the cover image picker.
 */

const MD_IMG_RE = /!\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g
const HTML_IMG_RE = /<img\s[^>]*?src=["']([^"']+)["']/gi

/** True for URLs the webview can render directly (remote or inline). */
export function isRenderableImageUrl(url: string): boolean {
  return /^https?:\/\//.test(url) || url.startsWith('data:image/')
}

/** True for image URLs supported by the project download command. */
export function isDownloadableImageUrl(url: string): boolean {
  return url.startsWith('https://')
}

/** Unique image URLs in source order from a markdown/HTML body. */
export function extractImageUrls(body: string): string[] {
  const urls: string[] = []
  const seen = new Set<string>()
  for (const re of [MD_IMG_RE, HTML_IMG_RE]) {
    re.lastIndex = 0
  }
  const matches: Array<{ index: number; url: string }> = []
  for (const match of body.matchAll(MD_IMG_RE)) {
    if (match[1]) matches.push({ index: match.index ?? 0, url: match[1] })
  }
  for (const match of body.matchAll(HTML_IMG_RE)) {
    if (match[1]) matches.push({ index: match.index ?? 0, url: match[1] })
  }
  matches.sort((a, b) => a.index - b.index)
  for (const { url } of matches) {
    if (seen.has(url)) continue
    seen.add(url)
    urls.push(url)
  }
  return urls
}

/** Image URLs on a single line of text (for inline preview decorations). */
export function imageUrlsOnLine(lineText: string): string[] {
  return extractImageUrls(lineText).filter(isRenderableImageUrl)
}

/**
 * Repo-relative destination for downloading a remote image as a cover:
 * `<coverDir>/<yyyy>/<mm>/<basename>`, reusing the year/month from the URL
 * path when it has one (CDN keys do), otherwise the current date.
 */
export function coverDestinationFor(
  url: string,
  coverDir: string,
  now: Date = new Date()
): string {
  const parsed = new URL(url)
  const base = parsed.pathname.split('/').pop() || 'cover.img'
  const safe = base.replace(/[^A-Za-z0-9._-]/g, '-')
  const dated = parsed.pathname.match(/\/((?:19|20)\d{2})\/(\d{2})\//)
  const yyyy = dated?.[1] ?? String(now.getFullYear())
  const mm = dated?.[2] ?? String(now.getMonth() + 1).padStart(2, '0')
  return `${coverDir.replace(/\/+$/, '')}/${yyyy}/${mm}/${safe}`
}

/** Adds a numeric suffix before the extension to avoid replacing a cover. */
export function numberedCoverDestination(
  destination: string,
  index: number
): string {
  if (index < 2) return destination

  const slash = destination.lastIndexOf('/')
  const dot = destination.lastIndexOf('.')
  const hasExtension = dot > slash
  const stem = hasExtension ? destination.slice(0, dot) : destination
  const extension = hasExtension ? destination.slice(dot) : ''
  return `${stem}-${index}${extension}`
}

/** Finds a destination that does not already exist. */
export async function findAvailableCoverDestination(
  destination: string,
  pathExists: (path: string) => Promise<boolean>
): Promise<string> {
  let index = 1
  let candidate = destination
  while (await pathExists(candidate)) {
    index += 1
    candidate = numberedCoverDestination(destination, index)
  }
  return candidate
}

/** File identity guard used after asynchronous image work. */
export function isSameFileIdentity(
  expected: { id: string; path: string } | null | undefined,
  actual: { id: string; path: string } | null | undefined
): boolean {
  return Boolean(
    expected &&
    actual &&
    expected.id === actual.id &&
    expected.path === actual.path
  )
}

/** Cover size spec: wide covers need 2400x1260, square ones 1600x1600. */
export function meetsCoverSpec(width: number, height: number): boolean {
  return (width >= 2400 && height >= 1260) || (width >= 1600 && height >= 1600)
}
