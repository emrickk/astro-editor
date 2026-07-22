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

/**
 * Parses preflight output. File paths are the indented lines before the
 * digest line; `empty` is set when no digest is present (a successful run
 * with nothing to ship prints no digest).
 */
export function parsePreflightOutput(output: string): PreflightResult {
  const lines = output.split('\n')
  const digestMatch = output.match(/^\s*changeset digest:\s*(\S+)\s*$/m)
  const digest = digestMatch?.[1] ?? null

  const files: string[] = []
  for (const line of lines) {
    if (/^\s*changeset digest:/.test(line)) break
    const match = line.match(/^\s+(\S+)$/)
    if (match?.[1]) files.push(match[1])
  }

  return { digest, files, empty: digest === null }
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
  { digest, files }: { digest?: string; files?: string[] }
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
        flag
          ? quoted.map(f => `${flag} ${f}`).join(' ')
          : quoted.join(' ')
    )
  }
  return command
}
