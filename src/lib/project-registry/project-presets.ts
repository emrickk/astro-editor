import { commands } from '@/lib/bindings'
import type { ProjectSettings } from './types'

interface PackageJsonShape {
  name?: unknown
  scripts?: unknown
}

const WLOG_PRESET: Partial<ProjectSettings> = {
  imageDropCommand: 'node scripts/images/drop.mjs',
  coverImagesDirectory: 'src/assets/hero',
  pullCommand: 'git pull --ff-only',
  publishPreflightCommand: 'npm run ship -- --preflight --fast --only {files}',
  publishConfirmCommand:
    'npm run ship -- --yes --fast --digest {digest} --only {files}',
  publishAutoConfirm: true,
}

/**
 * Built-in presets are intentionally allow-listed instead of accepting shell
 * commands from a repository config file. Opening an unfamiliar project must
 * never silently grant that project an arbitrary command surface.
 */
export function detectBuiltInProjectPreset(
  packageJson: PackageJsonShape
): Partial<ProjectSettings> | null {
  if (packageJson.name !== 'theneverless') return null
  if (
    !packageJson.scripts ||
    typeof packageJson.scripts !== 'object' ||
    Array.isArray(packageJson.scripts)
  ) {
    return null
  }

  const scripts = packageJson.scripts as Record<string, unknown>
  if (
    scripts.ship !== 'node scripts/ship.mjs' ||
    scripts['preview-posts'] !== 'node scripts/preview-posts.mjs' ||
    scripts.images !== 'node scripts/images/process.mjs'
  ) {
    return null
  }

  return { ...WLOG_PRESET }
}

export async function loadBuiltInProjectPreset(
  projectPath: string
): Promise<Partial<ProjectSettings>> {
  const packageJsonPath = `${projectPath.replace(/\/+$/, '')}/package.json`
  const result = await commands.readFileContent(packageJsonPath, projectPath)
  if (result.status === 'error') return {}

  try {
    const packageJson = JSON.parse(result.data) as PackageJsonShape
    return detectBuiltInProjectPreset(packageJson) ?? {}
  } catch {
    return {}
  }
}
