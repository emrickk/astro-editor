import React from 'react'
import { Switch } from '@/components/ui/switch'
import {
  Field,
  FieldLabel,
  FieldDescription,
  FieldContent,
} from '@/components/ui/field'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { usePreferences } from '../../../hooks/usePreferences'
import { SettingsSection } from '../SettingsSection'
import { PreferencesTextInput } from '../PreferencesTextInput'
import { DocsLink } from '../DocsLink'
import { DOCS_URLS } from '../../../lib/docs-urls'

export const ProjectSettingsPane: React.FC = () => {
  const { currentProjectSettings, updateProject, projectName, globalSettings } =
    usePreferences()

  const handlePathOverrideChange = (
    key: 'contentDirectory' | 'assetsDirectory' | 'mdxComponentsDirectory',
    value: string
  ) => {
    void updateProject({
      pathOverrides: {
        ...currentProjectSettings?.pathOverrides,
        [key]: value || undefined, // Remove empty strings
      },
    })
  }

  const handleDefaultFileTypeChange = (value: string) => {
    void updateProject({
      ...currentProjectSettings,
      defaultFileType:
        value === 'inherited' ? undefined : (value as 'md' | 'mdx'),
    })
  }

  const handleAbsolutePathsChange = (checked: boolean) => {
    void updateProject({
      useAbsoluteAssetPaths: checked,
    })
  }

  const handleImageDropCommandChange = (value: string) => {
    void updateProject({
      imageDropCommand: value.trim() || undefined, // Remove empty strings
    })
  }

  const handleCommandChange = (
    key:
      | 'pullCommand'
      | 'publishPreflightCommand'
      | 'publishReviewCommand'
      | 'publishConfirmCommand',
    value: string
  ) => {
    void updateProject({
      [key]: value.trim() || undefined, // Remove empty strings
    })
  }

  const handleAutoConfirmChange = (checked: boolean) => {
    void updateProject({
      publishAutoConfirm: checked || undefined, // Remove when off
    })
  }

  return (
    <div className="space-y-6">
      <div className="rounded-lg border bg-muted/50 p-4 mb-6">
        <h2 className="text-base font-semibold mb-1 text-heading">
          Project Settings
          {projectName && (
            <span className="text-muted-foreground font-normal ml-2">
              · {projectName}
            </span>
          )}
        </h2>
        <p className="text-sm text-muted-foreground">
          These settings apply to this project only. If not set, default values
          are used. Collection-specific overrides can be configured in the
          Collections tab.
        </p>
      </div>

      <SettingsSection title="Path Overrides">
        <p className="text-sm text-muted-foreground -mt-3 mb-2">
          Override default Astro paths for{' '}
          <span className="font-medium">{projectName}</span>. Paths should be
          relative to the project root.{' '}
          <DocsLink href={DOCS_URLS.overrides}>Learn more</DocsLink>
        </p>

        <Field>
          <FieldLabel>Content Directory</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={
                currentProjectSettings?.pathOverrides?.contentDirectory || ''
              }
              onCommit={value =>
                handlePathOverrideChange('contentDirectory', value)
              }
              placeholder="src/content/"
            />
            <FieldDescription>
              Path to Astro content directory (default: src/content/)
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <FieldLabel>Assets Directory</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={
                currentProjectSettings?.pathOverrides?.assetsDirectory || ''
              }
              onCommit={value =>
                handlePathOverrideChange('assetsDirectory', value)
              }
              placeholder="src/assets/"
            />
            <FieldDescription>
              Path to Astro assets directory (default: src/assets/)
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <FieldLabel>MDX Components Directory</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={
                currentProjectSettings?.pathOverrides?.mdxComponentsDirectory ||
                ''
              }
              onCommit={value =>
                handlePathOverrideChange('mdxComponentsDirectory', value)
              }
              placeholder="src/components/mdx/"
            />
            <FieldDescription>
              Path to components for use in MDX files (default:
              src/components/mdx/)
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <div className="flex items-center justify-between">
            <div className="flex-1">
              <FieldLabel>Use Absolute Paths for Images</FieldLabel>
              <FieldDescription>
                By default, images use paths relative to the current file (e.g.,{' '}
                <code className="text-xs">../../assets/image.png</code>),
                matching Astro's conventions. Enable this to override and use
                absolute paths from project root instead (e.g.,{' '}
                <code className="text-xs">/src/assets/image.png</code>).
              </FieldDescription>
            </div>
            <Switch
              checked={currentProjectSettings?.useAbsoluteAssetPaths ?? false}
              onCheckedChange={handleAbsolutePathsChange}
            />
          </div>
        </Field>
      </SettingsSection>

      <SettingsSection title="Images">
        <Field>
          <FieldLabel>Image Drop Command</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={currentProjectSettings?.imageDropCommand || ''}
              onCommit={handleImageDropCommandChange}
              placeholder="node scripts/images/drop.mjs"
            />
            <FieldDescription>
              Optional shell command run for images dropped into the editor,
              instead of copying them into the assets directory. It runs from
              the project root with the image path appended as one argument,
              and whatever it prints to stdout is inserted at the cursor (for
              example a markdown snippet pointing at a CDN URL). Leave empty
              to keep the default copy-to-assets behaviour.
            </FieldDescription>
          </FieldContent>
        </Field>
      </SettingsSection>

      <SettingsSection title="Sync & Publish">
        <Field>
          <FieldLabel>Pull Command</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={currentProjectSettings?.pullCommand || ''}
              onCommit={value => handleCommandChange('pullCommand', value)}
              placeholder="git pull --ff-only"
            />
            <FieldDescription>
              Command run by the Pull button (default: git pull --ff-only)
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <FieldLabel>Publish Preflight Command</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={currentProjectSettings?.publishPreflightCommand || ''}
              onCommit={value =>
                handleCommandChange('publishPreflightCommand', value)
              }
              placeholder="npm run ship -- --preflight"
            />
            <FieldDescription>
              Computes the change set before publishing. Must print a
              &quot;changeset digest: &lt;token&gt;&quot; line; a non-zero
              exit aborts with its own explanation. Publishing is enabled
              only when this and the confirm command are set. Include
              {' {files}'} to scope the publish to the currently open post
              and its translation siblings (repo-relative paths; a flag
              directly before it is repeated per file).
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <FieldLabel>Publish Review Command</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={currentProjectSettings?.publishReviewCommand || ''}
              onCommit={value =>
                handleCommandChange('publishReviewCommand', value)
              }
              placeholder="npm run preview-posts -- --port 4326"
            />
            <FieldDescription>
              Optional long-running review server started while the publish
              dialog is open (e.g. a production preview); it is stopped after
              your decision.
            </FieldDescription>
          </FieldContent>
        </Field>

        <Field>
          <div className="flex items-center justify-between">
            <div className="flex-1">
              <FieldLabel>One-Click Publish</FieldLabel>
              <FieldDescription>
                Skip the confirmation dialog and review server: clicking
                Publish runs the pipeline immediately after a successful
                preflight, with progress shown in a toast. All automated
                checks still run, and failures still open a dialog.
              </FieldDescription>
            </div>
            <Switch
              checked={currentProjectSettings?.publishAutoConfirm ?? false}
              onCheckedChange={handleAutoConfirmChange}
            />
          </div>
        </Field>

        <Field>
          <FieldLabel>Publish Confirm Command</FieldLabel>
          <FieldContent>
            <PreferencesTextInput
              value={currentProjectSettings?.publishConfirmCommand || ''}
              onCommit={value =>
                handleCommandChange('publishConfirmCommand', value)
              }
              placeholder="npm run ship -- --yes --digest {digest}"
            />
            <FieldDescription>
              Runs after you approve; {'{digest}'} is replaced with the
              preflight token so the pipeline can verify nothing changed
              since the review, and {'{files}'} with the same scoped paths
              as the preflight command.
            </FieldDescription>
          </FieldContent>
        </Field>
      </SettingsSection>

      <SettingsSection title="File Defaults">
        <Field>
          <FieldLabel>Default File Type for New Files</FieldLabel>
          <FieldContent>
            <Select
              value={currentProjectSettings?.defaultFileType || 'inherited'}
              onValueChange={handleDefaultFileTypeChange}
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="inherited">
                  <span className="text-muted-foreground">
                    Use global default:{' '}
                    {globalSettings?.general?.defaultFileType === 'mdx'
                      ? 'MDX'
                      : 'Markdown'}
                  </span>
                </SelectItem>
                <SelectItem value="md">Markdown (.md)</SelectItem>
                <SelectItem value="mdx">MDX (.mdx)</SelectItem>
              </SelectContent>
            </Select>
            <FieldDescription>
              File type used when creating new files in this project
            </FieldDescription>
          </FieldContent>
        </Field>
      </SettingsSection>
    </div>
  )
}
