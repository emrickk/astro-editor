import { describe, it, expect } from 'vitest'
import { parsePreflightOutput, confirmCommandFor } from './publish'

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

describe('confirmCommandFor', () => {
  it('substitutes the digest placeholder', () => {
    expect(
      confirmCommandFor('npm run ship -- --yes --digest {digest}', 'ab12cd34')
    ).toBe('npm run ship -- --yes --digest ab12cd34')
  })
})
