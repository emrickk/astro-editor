import '@testing-library/jest-dom'
import { vi } from 'vitest'

function createMemoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() {
      return values.size
    },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => values.delete(key),
    setItem: (key, value) => values.set(key, String(value)),
  }
}

// Node's experimental localStorage global can shadow jsdom with `undefined`.
// Install one deterministic browser-compatible store before app modules load.
const testStorage = createMemoryStorage()
Object.defineProperty(window, 'localStorage', {
  configurable: true,
  value: testStorage,
})
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: testStorage,
})

// Mock Tauri API calls for testing
const mockInvoke = vi.fn()
const mockListen = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mockInvoke,
}))

vi.mock('@tauri-apps/api/event', () => ({
  listen: mockListen,
}))

// Mock project registry manager for testing
const mockProjectRegistryManager = {
  init: vi.fn().mockResolvedValue(undefined),
  initialize: vi.fn().mockResolvedValue(undefined),
  registerProject: vi.fn().mockImplementation((path: string) => {
    // Return path as project ID for tests
    return Promise.resolve(path.replace(/[/\\]/g, '-').replace(/^-/, ''))
  }),
  getEffectiveSettings: vi.fn().mockResolvedValue({
    pathOverrides: {
      contentDirectory: 'src/content/',
      assetsDirectory: 'src/assets/',
      mdxComponentsDirectory: 'src/components/mdx/',
    },
    frontmatterMappings: {
      title: 'title',
      description: 'description',
      publishedDate: 'date',
      draft: 'draft',
    },
  }),
  updateGlobalSettings: vi.fn().mockResolvedValue(undefined),
  updateProjectSettings: vi.fn().mockResolvedValue(undefined),
  getLastOpenedProjectId: vi.fn().mockReturnValue(null),
  getProjectData: vi.fn().mockResolvedValue(null),
  getGlobalSettings: vi.fn().mockReturnValue({
    general: { ideCommand: '' },
  }),
  getRegistry: vi.fn().mockReturnValue({
    projects: {},
    lastOpenedProject: null,
  }),
}

vi.mock('../lib/project-registry', async importOriginal => {
  // Keep the module's pure helpers (getEffectiveContentDirectory,
  // getCollectionSettings, defaults) real; mock only the manager singleton.
  const actual = await importOriginal<object>()
  return {
    ...actual,
    projectRegistryManager: mockProjectRegistryManager,
    ProjectRegistryManager: vi
      .fn()
      .mockImplementation(() => mockProjectRegistryManager),
  }
})

// Make mocks available globally for tests
globalThis.mockTauri = {
  invoke: mockInvoke,
  listen: mockListen,
  reset: () => {
    mockInvoke.mockReset()
    mockListen.mockReset()
    // Reset project registry mocks
    mockProjectRegistryManager.init.mockClear()
    mockProjectRegistryManager.initialize.mockClear()
    mockProjectRegistryManager.registerProject.mockClear()
    mockProjectRegistryManager.getEffectiveSettings.mockClear()
    mockProjectRegistryManager.updateGlobalSettings.mockClear()
    mockProjectRegistryManager.updateProjectSettings.mockClear()
    mockProjectRegistryManager.getLastOpenedProjectId.mockClear()
    mockProjectRegistryManager.getProjectData.mockClear()
    mockProjectRegistryManager.getGlobalSettings.mockClear()
    mockProjectRegistryManager.getRegistry.mockClear()
  },
}
