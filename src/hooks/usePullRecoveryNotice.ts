import { useCallback, useEffect, useRef } from 'react'
import { commands } from '@/lib/bindings'
import { toast } from '@/lib/toast'
import { useProjectStore } from '@/store/projectStore'
import { usePublishStore, type PublishStage } from '@/store/publishStore'

function recoveryToastId(projectPath: string): string {
  return `pull-recoveries:${projectPath}`
}

export function usePullRecoveryNotice(onReview: () => void): void {
  const projectPath = useProjectStore(state => state.projectPath)
  const publishStage = usePublishStore(state => state.stage)
  const requestIdRef = useRef(0)
  const onReviewRef = useRef(onReview)
  const previousStageRef = useRef<PublishStage>('idle')
  const pullingProjectRef = useRef<string | null>(null)

  useEffect(() => {
    onReviewRef.current = onReview
  }, [onReview])

  const checkForRecoveries = useCallback(async (path: string) => {
    const requestId = ++requestIdRef.current
    try {
      const result = await commands.listPullRecoveries(path)
      if (
        requestId !== requestIdRef.current ||
        useProjectStore.getState().projectPath !== path ||
        result.status === 'error'
      ) {
        return
      }

      const toastId = recoveryToastId(path)
      if (result.data.length === 0) {
        toast.dismiss(toastId)
        return
      }

      const count = result.data.length
      const attentionCount = result.data.filter(
        recovery => recovery.status === 'needsAttention'
      ).length
      const title =
        attentionCount > 0
          ? `${attentionCount} Pull safety ${attentionCount === 1 ? 'copy needs' : 'copies need'} attention`
          : `${count} Pull safety ${count === 1 ? 'copy' : 'copies'} available`
      toast.warning(title, {
        id: toastId,
        description:
          attentionCount > 0
            ? 'Pull stopped before these saved versions were sealed. Restore and inspect them before deleting anything.'
            : 'Newer file versions were preserved. Review them before deleting anything.',
        duration: 12_000,
        action: {
          label: 'Review copies',
          onClick: () => {
            if (useProjectStore.getState().projectPath === path) {
              onReviewRef.current()
            }
          },
        },
      })
    } catch {
      // Startup discovery is advisory. The Project Settings pane exposes a
      // visible retry if the owner opens recovery history directly.
    }
  }, [])

  useEffect(() => {
    if (!projectPath) return
    void checkForRecoveries(projectPath)
    const toastId = recoveryToastId(projectPath)

    return () => {
      requestIdRef.current += 1
      toast.dismiss(toastId)
    }
  }, [checkForRecoveries, projectPath])

  useEffect(() => {
    const previousStage = previousStageRef.current
    previousStageRef.current = publishStage

    if (publishStage === 'pulling' && previousStage !== 'pulling') {
      pullingProjectRef.current = projectPath
      return
    }
    if (previousStage !== 'pulling' || publishStage === 'pulling') return

    const pulledProject = pullingProjectRef.current
    pullingProjectRef.current = null
    if (pulledProject && pulledProject === projectPath) {
      void checkForRecoveries(pulledProject)
    }
  }, [checkForRecoveries, projectPath, publishStage])
}
