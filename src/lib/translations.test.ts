import { describe, it, expect } from 'vitest'
import { getSiblingCandidatePaths } from './translations'

describe('getSiblingCandidatePaths', () => {
  it('maps a zh sibling back to the primary file', () => {
    expect(
      getSiblingCandidatePaths('/p/src/content/posts/city-walk.zh.md')
    ).toEqual([
      '/p/src/content/posts/city-walk.md',
      '/p/src/content/posts/city-walk.mdx',
    ])
  })

  it('maps an en sibling back to the primary file', () => {
    expect(getSiblingCandidatePaths('/p/posts/city-walk.en.mdx')).toEqual([
      '/p/posts/city-walk.mdx',
      '/p/posts/city-walk.md',
    ])
  })

  it('maps a primary file to lang siblings, same extension first', () => {
    expect(getSiblingCandidatePaths('/p/posts/city-walk.md')).toEqual([
      '/p/posts/city-walk.zh.md',
      '/p/posts/city-walk.en.md',
      '/p/posts/city-walk.zh.mdx',
      '/p/posts/city-walk.en.mdx',
    ])
  })

  it('treats non-lang dotted names as primaries', () => {
    expect(getSiblingCandidatePaths('/p/posts/v2.0-notes.md')[0]).toBe(
      '/p/posts/v2.0-notes.zh.md'
    )
  })

  it('returns [] for non-markdown files', () => {
    expect(getSiblingCandidatePaths('/p/posts/image.png')).toEqual([])
  })

  it('handles filenames without a directory', () => {
    expect(getSiblingCandidatePaths('post.zh.md')).toEqual([
      'post.md',
      'post.mdx',
    ])
  })
})
