import { useMutation, useQueryClient } from '@tanstack/react-query'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState, type FormEvent } from 'react'
import { Button } from '@/components/ui/button'
import { createJob, jobsQueryOptions } from '@/lib/api'

export const Route = createFileRoute('/new')({
  component: NewJobPage,
})

function NewJobPage() {
  const [prompt, setPrompt] = useState('')
  const [mode, setMode] = useState<'job' | 'loop'>('job')
  const [milestone, setMilestone] = useState('')
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const mutation = useMutation({
    mutationFn: createJob,
    onSuccess: ({ id }) => {
      void queryClient.invalidateQueries({ queryKey: jobsQueryOptions.queryKey })
      void navigate({ to: '/jobs/$jobId', params: { jobId: String(id) } })
    },
  })

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!prompt.trim()) return
    mutation.mutate({ prompt, mode, milestone })
  }

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-2xl font-semibold">Novo job</h1>
        <p className="text-sm text-muted-foreground">
          {mode === 'job'
            ? 'Job unico: planeja e executa as tarefas no checkout configurado, sem commit/push.'
            : 'Loop de entrega: observa o GitHub, o brain escolhe o proximo item, um job por item em worktree proprio, com PR, review e merge conforme a politica do projeto.'}
        </p>
      </div>

      <form onSubmit={onSubmit} className="space-y-3">
        <div className="flex flex-wrap items-center gap-2" role="radiogroup" aria-label="Modo">
          <Button
            type="button"
            variant={mode === 'job' ? 'default' : 'outline'}
            onClick={() => setMode('job')}
            role="radio"
            aria-checked={mode === 'job'}
          >
            Job unico
          </Button>
          <Button
            type="button"
            variant={mode === 'loop' ? 'default' : 'outline'}
            onClick={() => setMode('loop')}
            role="radio"
            aria-checked={mode === 'loop'}
          >
            Loop de entrega
          </Button>
          {mode === 'loop' && (
            <input
              value={milestone}
              onChange={(e) => setMilestone(e.target.value)}
              placeholder='Milestone (ex.: 5 = "M5 ...", ou titulo exato) — opcional'
              className="min-w-64 flex-1 rounded-md border bg-background px-3 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
              maxLength={120}
            />
          )}
        </div>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          rows={8}
          maxLength={20000}
          placeholder={
            mode === 'loop'
              ? 'Sincronize o repositorio, veja o ultimo PR mergeado e continue as tarefas da Milestone 5 ate concluir.'
              : 'Analise o projeto e implemente a funcionalidade X com testes.'
          }
          className="w-full rounded-md border bg-background p-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
          disabled={mutation.isPending}
        />

        {mutation.isError && <p className="text-sm text-destructive">{mutation.error.message}</p>}

        <Button type="submit" disabled={mutation.isPending || !prompt.trim()}>
          {mutation.isPending ? 'Enviando…' : 'Enviar'}
        </Button>
      </form>
    </div>
  )
}
