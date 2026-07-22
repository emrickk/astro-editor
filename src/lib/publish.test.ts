import { describe, it, expect } from 'vitest'
import {
  parsePreflightOutput,
  expandPublishCommand,
  commandWantsFiles,
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

describe('commandWantsFiles', () => {
  it('detects the files placeholder', () => {
    expect(commandWantsFiles('ship --only {files}')).toBe(true)
    expect(commandWantsFiles('ship --preflight')).toBe(false)
    expect(commandWantsFiles(undefined)).toBe(false)
  })
})
