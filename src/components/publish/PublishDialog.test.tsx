import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { usePublishStore } from '../../store/publishStore'
import { PublishDialog } from './PublishDialog'

function showError(operation: 'pull' | 'publish', error: string): void {
  usePublishStore.setState({
    stage: 'error',
    error,
    errorOperation: operation,
    log: [],
  })
}

function showReloadWarning(): void {
  usePublishStore.setState({
    stage: 'reload-warning',
    error: null,
    errorOperation: 'pull',
    completionLabel: 'Pull complete',
    completionSummary: 'Updated main',
    reloadWarning: 'The open file could not be reloaded from disk.',
    log: [],
  })
}

describe('PublishDialog operation errors', () => {
  afterEach(() => {
    cleanup()
    usePublishStore.setState({
      stage: 'idle',
      error: null,
      errorOperation: null,
      completionLabel: null,
      completionSummary: null,
      reloadWarning: null,
      log: [],
    })
  })

  it('labels a pull failure as Pull instead of Publish', () => {
    showError('pull', 'Recovery: refs/astro-editor/pull-recovery/abc')

    render(<PublishDialog />)

    expect(screen.getByText('Pull failed')).toBeInTheDocument()
    expect(
      screen.getByText(
        'Pull stopped with an error. Review the details and recovery guidance below before trying again.'
      )
    ).toBeInTheDocument()
    expect(screen.queryByText('Publish stopped')).not.toBeInTheDocument()
  })

  it('keeps publish-specific guidance for publish failures', () => {
    showError('publish', 'Release check failed')

    render(<PublishDialog />)

    expect(screen.getByText('Publish stopped')).toBeInTheDocument()
    expect(
      screen.getByText(
        'The pipeline explained why below. Nothing was pushed unless the log says otherwise.'
      )
    ).toBeInTheDocument()
    expect(screen.queryByText('Pull failed')).not.toBeInTheDocument()
  })

  it('preserves a successful pull result when only the editor reload fails', () => {
    showReloadWarning()

    render(<PublishDialog />)

    expect(screen.getByText('Pull complete')).toBeInTheDocument()
    expect(screen.getByText('Updated main')).toBeInTheDocument()
    expect(
      screen.getByText('The open file could not be reloaded from disk.')
    ).toBeInTheDocument()
    expect(screen.queryByText('Publish stopped')).not.toBeInTheDocument()
    expect(screen.queryByText('Pull failed')).not.toBeInTheDocument()
  })

  it('shows an in-progress state while review shutdown is being confirmed', () => {
    usePublishStore.setState({
      stage: 'cancelling',
      error: null,
    })

    render(<PublishDialog />)

    expect(screen.getByText('Stopping review…')).toBeInTheDocument()
    expect(
      screen.getByText('Confirming the review process has exited…')
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Stopping…' })).toBeDisabled()
  })

  it('offers a retry when review shutdown cannot be confirmed', () => {
    usePublishStore.setState({
      stage: 'cancelling',
      error: 'The review process is still running.',
    })

    render(<PublishDialog />)

    expect(screen.getByText('Review is still running')).toBeInTheDocument()
    expect(
      screen.getByText('The review process is still running.')
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry stop' })).toBeEnabled()
  })
})
