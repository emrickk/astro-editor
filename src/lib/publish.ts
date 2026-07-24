/**
 * Pull & publish support.
 *
 * Publishing drives a project-provided two-phase pipeline (configured in
 * project settings), so the editor never invents its own git behaviour:
 *
 * 1. `publishPreflightCommand` computes the change set and prints it along
 *    with a line `changeset digest: <token>`; a non-zero exit aborts with
 *    the pipeline's own explanation.
 * 2. `publishReviewCommand` (optional) serves a review of the pending
 *    changes (long-running; the editor stops it after the decision).
 * 3. `publishConfirmCommand` runs on approval, with `{digest}` replaced by
 *    the preflight token so the pipeline can verify freshness.
 *
 * Commands may also contain `{files}`: it expands to the repo-relative
 * paths of the currently open post and its existing translation siblings,
 * shell-quoted, so a pipeline can scope the publish to just that post.
 */

export const DEFAULT_PULL_COMMAND = 'git pull --ff-only'

export interface PreflightResult {
  /** Freshness token parsed from a `changeset digest: <token>` line */
  digest: string | null
  /** Changed file paths (indented lines preceding the digest line) */
  files: string[]
  /** True when the pipeline reported nothing to publish */
  empty: boolean
}

export class PreflightProtocolError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'PreflightProtocolError'
  }
}

const EMPTY_PREFLIGHT_RE =
  /^\s*(?:nothing to (?:publish|ship)(?: for .+)?|no changes(?: to publish)?|already up[ -]to[ -]date)\s*[.!]?\s*$/im

/**
 * Parses the small preflight protocol used by publish commands. A successful
 * preflight must explicitly report either a changeset digest or a recognized
 * "nothing to ship" line. Treating arbitrary output as an empty changeset
 * would hide broken or misconfigured commands.
 */
export function parsePreflightOutput(output: string): PreflightResult {
  const lines = output.split('\n')
  const digestMatches = [
    ...output.matchAll(/^\s*changeset digest:\s*(\S+)\s*$/gm),
  ]

  if (digestMatches.length === 0) {
    if (EMPTY_PREFLIGHT_RE.test(output)) {
      return { digest: null, files: [], empty: true }
    }
    throw new PreflightProtocolError(
      'Publish preflight returned an unrecognized response. It must print either "changeset digest: <token>" with the changed files or "nothing to ship".'
    )
  }

  if (digestMatches.length > 1) {
    throw new PreflightProtocolError(
      'Publish preflight returned more than one changeset digest.'
    )
  }

  const digest = digestMatches[0]?.[1] ?? null
  if (digest && !/^[A-Za-z0-9._:-]+$/.test(digest)) {
    throw new PreflightProtocolError(
      'Publish preflight returned an unsafe changeset digest.'
    )
  }

  const files: string[] = []
  for (const line of lines) {
    if (/^\s*changeset digest:/.test(line)) break
    const match = line.match(/^\s+(\S+)$/)
    if (match?.[1]) files.push(match[1])
  }

  if (!digest || files.length === 0) {
    throw new PreflightProtocolError(
      'Publish preflight returned a digest without any changed files.'
    )
  }

  return { digest, files, empty: false }
}

export interface PublishCommandValidation {
  valid: boolean
  error: string | null
}

/** Validates the two-phase command contract before any command is run. */
export function validatePublishCommands(
  preflightTemplate: string | undefined,
  confirmTemplate: string | undefined,
  reviewTemplate?: string
): PublishCommandValidation {
  const preflight = preflightTemplate?.trim()
  const confirm = confirmTemplate?.trim()
  if (!preflight || !confirm) {
    return {
      valid: false,
      error: 'Configure both publish preflight and confirm commands.',
    }
  }
  if (!confirm.includes('{digest}')) {
    return {
      valid: false,
      error: 'The publish confirm command must include {digest}.',
    }
  }
  if (commandWantsFiles(preflight) !== commandWantsFiles(confirm)) {
    return {
      valid: false,
      error:
        'The publish preflight and confirm commands must either both include {files} or both omit it.',
    }
  }
  if (commandWantsFiles(reviewTemplate) && !commandWantsFiles(preflight)) {
    return {
      valid: false,
      error:
        'A review command using {files} requires {files} in both publish commands.',
    }
  }
  return { valid: true, error: null }
}

/** Turns common safe git refusals into an actionable, non-destructive message. */
export function formatPullError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  if (
    /local changes to the following files would be overwritten by merge/i.test(
      message
    )
  ) {
    const fileMatch = message.match(
      /overwritten by merge:\s*([\s\S]*?)\s*Please commit/i
    )
    const files = fileMatch?.[1]
      ?.split(/\s+/)
      .map(file => file.trim())
      .filter(Boolean)
    const fileSummary = files?.length ? ` (${files.join(', ')})` : ''
    return `Pull stopped to protect your local changes${fileSummary}. Reconcile those edits with GitHub (or commit them) before pulling again. The app did not stash or overwrite anything.`
  }
  if (
    /not possible to fast-forward|diverging branches|non-fast-forward/i.test(
      message
    )
  ) {
    return 'Pull stopped because local and remote history have diverged. Nothing was changed. Reconcile the branches in Git, then try again.'
  }
  return message || 'Unknown error'
}

export interface PublishFinding {
  file: string
  message: string
}

export type PublishCompletion = 'deployed' | 'pushed' | 'completed'

/**
 * Classifies what a successful command actually proved. A zero exit code can
 * mean the commit was pushed while the deploy watcher was unavailable, so the
 * UI must not call that state "Published" unless deployment was confirmed.
 */
export function classifyPublishCompletion(output: string): PublishCompletion {
  if (/^deploy complete\b/im.test(output)) return 'deployed'
  if (/^pushed\s+\S+/im.test(output)) return 'pushed'
  return 'completed'
}

/**
 * Extracts per-file findings from pipeline error output. Fast checks and
 * schema validators print findings as `  <path>.md: <message>` lines; these
 * become the prominent "what needs fixing" list in the error dialog, ahead
 * of the raw log tail.
 */
export function extractFindings(errorText: string): PublishFinding[] {
  const findings: PublishFinding[] = []
  const seen = new Set<string>()
  for (const line of errorText.split('\n')) {
    const match = line.match(/^\s*(\S+\.(?:md|mdx)):\s+(.+)$/)
    if (!match) continue
    const key = `${match[1]}|${match[2]}`
    if (seen.has(key)) continue
    seen.add(key)
    findings.push({ file: match[1]!, message: match[2]! })
  }
  return findings
}

/** Quotes a path for safe interpolation into a POSIX shell command line. */
export function shellQuote(path: string): string {
  return `'${path.replaceAll("'", `'\\''`)}'`
}

/** True when the template scopes the publish to specific files. */
export function commandWantsFiles(template: string | undefined): boolean {
  return Boolean(template?.includes('{files}'))
}

/**
 * Substitutes command placeholders: `{digest}` with the preflight token,
 * `{files}` with the shell-quoted, space-joined scoped file paths. When
 * `{files}` directly follows an option flag, the flag is repeated for each
 * file (`--only {files}` becomes `--only 'a' --only 'b'`), matching
 * parseArgs-style multiple options.
 */
export function expandPublishCommand(
  template: string,
  { digest, files }: { digest?: string; files?: readonly string[] }
): string {
  let command = template
  if (digest !== undefined) {
    command = command.replaceAll('{digest}', digest)
  }
  if (files !== undefined) {
    const quoted = files.map(shellQuote)
    command = command.replace(
      /(--?[\w-]+)\s+\{files\}|\{files\}/g,
      (_match, flag: string | undefined) =>
        flag ? quoted.map(f => `${flag} ${f}`).join(' ') : quoted.join(' ')
    )
  }
  return command
}
