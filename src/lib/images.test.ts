import { describe, it, expect } from 'vitest'
import {
  extractImageUrls,
  imageUrlsOnLine,
  coverDestinationFor,
  meetsCoverSpec,
} from './images'

const BODY = `Some prose.

![alt text](https://cdn.theneverless.com/2026/07/sf-cafe.webp)

<div class="img-grid">
  <img src="https://cdn.theneverless.com/2026/07/sf-books.webp" alt="书架" loading="lazy" />
  <img src='https://cdn.theneverless.com/2026/07/sf-alley.webp' alt="小巷" />
</div>

![local](../../assets/hero/cover.webp)
![dup](https://cdn.theneverless.com/2026/07/sf-cafe.webp)
`

describe('extractImageUrls', () => {
  it('finds markdown and html images in source order, deduped', () => {
    expect(extractImageUrls(BODY)).toEqual([
      'https://cdn.theneverless.com/2026/07/sf-cafe.webp',
      'https://cdn.theneverless.com/2026/07/sf-books.webp',
      'https://cdn.theneverless.com/2026/07/sf-alley.webp',
      '../../assets/hero/cover.webp',
    ])
  })
})

describe('imageUrlsOnLine', () => {
  it('keeps only renderable urls', () => {
    expect(imageUrlsOnLine('![x](../../local.png) ![y](https://a.com/b.png)')).toEqual([
      'https://a.com/b.png',
    ])
  })
})

describe('coverDestinationFor', () => {
  it('reuses the year and month from a CDN key', () => {
    expect(
      coverDestinationFor(
        'https://cdn.theneverless.com/2026/07/sf-cafe.webp',
        'src/assets/hero'
      )
    ).toBe('src/assets/hero/2026/07/sf-cafe.webp')
  })

  it('falls back to the current date and sanitizes the name', () => {
    expect(
      coverDestinationFor(
        'https://example.com/images/My%20Photo.webp',
        'src/assets/hero/',
        new Date('2026-07-22T00:00:00Z')
      )
    ).toBe('src/assets/hero/2026/07/My-20Photo.webp')
  })
})

describe('meetsCoverSpec', () => {
  it('accepts wide and square covers, rejects small ones', () => {
    expect(meetsCoverSpec(2400, 1260)).toBe(true)
    expect(meetsCoverSpec(1600, 1600)).toBe(true)
    expect(meetsCoverSpec(1200, 900)).toBe(false)
    expect(meetsCoverSpec(2400, 800)).toBe(false)
  })
})
