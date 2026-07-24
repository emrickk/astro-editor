import { render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useProjectStore } from '@/store/projectStore'
import { PreferencesDialog } from './PreferencesDialog'

vi.mock('./panes/GeneralPane', () => ({
  GeneralPane: () => <div>General pane</div>,
}))
vi.mock('./panes/ProjectSettingsPane', () => ({
  ProjectSettingsPane: () => <div>Project recovery settings</div>,
}))
vi.mock('./panes/CollectionSettingsPane', () => ({
  CollectionSettingsPane: () => <div>Collection pane</div>,
}))
vi.mock('./panes/DebugPane', () => ({
  DebugPane: () => <div>Advanced pane</div>,
}))

describe('PreferencesDialog requested pane', () => {
  beforeEach(() => {
    useProjectStore.setState({
      currentProjectId: 'repo',
      projectPath: '/repo',
    })
  })

  it('opens a recovery notice directly on Project Settings', async () => {
    render(
      <PreferencesDialog open onOpenChange={vi.fn()} requestedPane="project" />
    )

    expect(await screen.findByText('Project recovery settings')).toBeVisible()
    expect(screen.getAllByText('Project Settings')).not.toHaveLength(0)
    expect(screen.queryByText('General pane')).not.toBeInTheDocument()
  })
})
