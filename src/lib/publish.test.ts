import { describe, it, expect } from 'vitest'
import {
  parsePreflightOutput,
  expandPublishCommand,
  commandWantsFiles,
  classifyPublishCompletion,
  extractFindings,
  formatPullError,
  validatePublishCommands,
} from './publish'

const SHIP_PREFLIGHT_OUTPUT = `post change(s) vs origin/main (2):
  src/content/posts/a-temple-fair-on-the-fairway.md
  src/content/posts/a-temple-fair-on-the-fairway.zh.md
changeset digest: ab12cd34ef56`

describe('parsePreflightOutput', () => {
  it('extracts digest and file list from ship --preflight output', () => {
    const result = parsePreflightOutput(SHIP_PREFLIGHT_OUTPUT)
    expect(result.digest).toBe('ab12cd34ef56')
    expect(result.files).toEqual([
      'src/content/posts/a-temple-fair-on-the-fairway.md',
      'src/content/posts/a-temple-fair-on-the-fairway.zh.md',
    ])
    expect(result.empty).toBe(false)
  })

  it('reports empty when there is no digest line', () => {
    const result = parsePreflightOutput('nothing to ship')
    expect(result.empty).toBe(true)
    expect(result.digest).toBeNull()
    expect(result.files).toEqual([])
  })

  it('rejects arbitrary successful output instead of treating it as empty', () => {
    expect(() => parsePreflightOutput('command completed somehow')).toThrow(
      /unrecognized response/
    )
  })

  it('rejects a digest without a file list', () => {
    expect(() => parsePreflightOutput('changeset digest: abc123')).toThrow(
      /without any changed files/
    )
  })

  it('rejects a digest containing shell syntax', () => {
    expect(() =>
      parsePreflightOutput(`post change:
  src/content/posts/a.md
changeset digest: abc;touch-pwned`)
    ).toThrow(/unsafe changeset digest/)
  })

  it('ignores lines after the digest line', () => {
    const result = parsePreflightOutput(
      `${SHIP_PREFLIGHT_OUTPUT}\n  some/trailing/noise.txt`
    )
    expect(result.files).toHaveLength(2)
  })
})

describe('expandPublishCommand', () => {
  it('substitutes the digest placeholder', () => {
    expect(
      expandPublishCommand('npm run ship -- --yes --digest {digest}', {
        digest: 'ab12cd34',
      })
    ).toBe('npm run ship -- --yes --digest ab12cd34')
  })

  it('repeats an option flag for each scoped file', () => {
    expect(
      expandPublishCommand('npm run ship -- --preflight --only {files}', {
        files: ['src/content/posts/a.md', 'src/content/posts/a.zh.md'],
      })
    ).toBe(
      "npm run ship -- --preflight --only 'src/content/posts/a.md' --only 'src/content/posts/a.zh.md'"
    )
  })

  it('joins files without flag repetition when standalone', () => {
    expect(
      expandPublishCommand('publish.sh {files}', { files: ["it's.md"] })
    ).toBe("publish.sh 'it'\\''s.md'")
  })

  it('substitutes digest and files together', () => {
    expect(
      expandPublishCommand('ship --yes --digest {digest} --only {files}', {
        digest: 'ff00',
        files: ['a.md'],
      })
    ).toBe("ship --yes --digest ff00 --only 'a.md'")
  })
})

describe('extractFindings', () => {
  it('pulls per-file findings out of fast-check output', () => {
    const output = `exit 1:
npm notice run theneverless@1.0.0 ship
fast checks failed:
  src/content/posts/2026-07-21.md: missing required field description
  src/content/posts/2026-07-21.md: pubDate does not parse as a date (nope)`
    expect(extractFindings(output)).toEqual([
      {
        file: 'src/content/posts/2026-07-21.md',
        message: 'missing required field description',
      },
      {
        file: 'src/content/posts/2026-07-21.md',
        message: 'pubDate does not parse as a date (nope)',
      },
    ])
  })

  it('dedupes repeated findings and ignores non-finding lines', () => {
    const output = `fast checks failed:
  a.md: missing required field title
  a.md: missing required field title
changeset digest: abc123`
    expect(extractFindings(output)).toHaveLength(1)
  })

  it('returns empty for unstructured errors', () => {
    expect(
      extractFindings('origin/main has 2 commit(s) not in local main')
    ).toEqual([])
  })
})

describe('classifyPublishCompletion', () => {
  it('only reports deployed when the command confirms deployment', () => {
    expect(
      classifyPublishCompletion(
        'pushed abc1234 to origin/main\ndeploy complete:\n  https://example.com/post/'
      )
    ).toBe('deployed')
  })

  it('distinguishes a push from an unverified deployment', () => {
    expect(
      classifyPublishCompletion(
        'pushed abc1234 to origin/main\ngh unavailable; watch the deploy'
      )
    ).toBe('pushed')
  })

  it('does not invent either guarantee for generic successful output', () => {
    expect(classifyPublishCompletion('command finished')).toBe('completed')
  })
})

describe('commandWantsFiles', () => {
  it('detects the files placeholder', () => {
    expect(commandWantsFiles('ship --only {files}')).toBe(true)
    expect(commandWantsFiles('ship --preflight')).toBe(false)
    expect(commandWantsFiles(undefined)).toBe(false)
  })
})

describe('validatePublishCommands', () => {
  it('requires a freshness digest in the confirm command', () => {
    expect(validatePublishCommands('ship --preflight', 'ship --yes')).toEqual({
      valid: false,
      error: 'The publish confirm command must include {digest}.',
    })
  })

  it('requires symmetric file scoping', () => {
    expect(
      validatePublishCommands(
        'ship --preflight --only {files}',
        'ship --yes --digest {digest}'
      ).error
    ).toMatch(/both include \{files\}/)
  })

  it('accepts a complete two-phase contract', () => {
    expect(
      validatePublishCommands(
        'ship --preflight --only {files}',
        'ship --yes --digest {digest} --only {files}'
      )
    ).toEqual({ valid: true, error: null })
  })
})

describe('formatPullError', () => {
  it('explains how to resolve a dirty-worktree refusal without stashing', () => {
    const error =
      new Error(`exit 1: Your local changes to the following files would be overwritten by merge:
  src/content/posts/a.md
    Please commit your changes or stash them before you merge. Aborting`)
    expect(formatPullError(error)).toBe(
      'Pull stopped to protect your local changes (src/content/posts/a.md). Reconcile those edits with GitHub (or commit them) before pulling again. The app did not stash or overwrite anything.'
    )
  })
})
