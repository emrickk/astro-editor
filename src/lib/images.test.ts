import { describe, it, expect } from 'vitest'
import {
  extractImageUrls,
  imageUrlsOnLine,
  coverDestinationFor,
  findAvailableCoverDestination,
  isDownloadableImageUrl,
  isSameFileIdentity,
  meetsCoverSpec,
  numberedCoverDestination,
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
    expect(
      imageUrlsOnLine('![x](../../local.png) ![y](https://a.com/b.png)')
    ).toEqual(['https://a.com/b.png'])
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

describe('downloadable image URLs', () => {
  it('matches the HTTPS-only backend contract', () => {
    expect(isDownloadableImageUrl('https://example.com/image.webp')).toBe(true)
    expect(isDownloadableImageUrl('http://example.com/image.webp')).toBe(false)
    expect(isDownloadableImageUrl('data:image/png;base64,abc')).toBe(false)
  })
})

describe('cover destination collisions', () => {
  it('adds a suffix before the extension', () => {
    expect(numberedCoverDestination('src/assets/cover.webp', 2)).toBe(
      'src/assets/cover-2.webp'
    )
    expect(numberedCoverDestination('src/assets/cover', 3)).toBe(
      'src/assets/cover-3'
    )
  })

  it('finds the first unused destination without overwriting', async () => {
    const existing = new Set([
      'src/assets/cover.webp',
      'src/assets/cover-2.webp',
    ])
    await expect(
      findAvailableCoverDestination('src/assets/cover.webp', path =>
        Promise.resolve(existing.has(path))
      )
    ).resolves.toBe('src/assets/cover-3.webp')
  })
})

describe('file identity guard', () => {
  const original = { id: 'posts/a', path: '/project/posts/a.md' }

  it('accepts only the exact file captured before async work', () => {
    expect(isSameFileIdentity(original, { ...original })).toBe(true)
    expect(
      isSameFileIdentity(original, {
        id: 'posts/b',
        path: '/project/posts/b.md',
      })
    ).toBe(false)
    expect(
      isSameFileIdentity(original, {
        id: original.id,
        path: '/other-project/posts/a.md',
      })
    ).toBe(false)
    expect(isSameFileIdentity(original, null)).toBe(false)
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
