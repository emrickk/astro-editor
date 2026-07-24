import { describe, expect, it } from 'vitest'
import { detectBuiltInProjectPreset } from './project-presets'

const wlogScripts = {
  ship: 'node scripts/ship.mjs',
  'preview-posts': 'node scripts/preview-posts.mjs',
  images: 'node scripts/images/process.mjs',
}

describe('detectBuiltInProjectPreset', () => {
  it('restores the safe W-Log workflow from the project identity', () => {
    const preset = detectBuiltInProjectPreset({
      name: 'theneverless',
      scripts: wlogScripts,
    })

    expect(preset).toEqual(
      expect.objectContaining({
        imageDropCommand: 'node scripts/images/drop.mjs',
        coverImagesDirectory: 'src/assets/hero',
        pullCommand: 'git pull --ff-only',
        publishAutoConfirm: true,
      })
    )
    expect(preset?.publishPreflightCommand).toContain('--fast')
    expect(preset?.publishPreflightCommand).toContain('{files}')
    expect(preset?.publishConfirmCommand).toContain('{digest}')
    expect(preset?.publishConfirmCommand).toContain('{files}')
  })

  it('does not enable commands based on the package name alone', () => {
    expect(
      detectBuiltInProjectPreset({
        name: 'theneverless',
        scripts: { ...wlogScripts, ship: 'malicious-command' },
      })
    ).toBeNull()
  })

  it('does not apply the personal preset to other Astro projects', () => {
    expect(
      detectBuiltInProjectPreset({ name: 'another-blog', scripts: wlogScripts })
    ).toBeNull()
  })
})
