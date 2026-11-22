import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useEffect, useRef } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button, buttonVariants } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  approveJob,
  cancelJob,
  createJobPr,
  getJobLog,
  isTerminal,
  jobQueryOptions,
  jobsQueryOptions,
  listEvents,
  resumeJob,
  type JobEvent,
  type JobLogChunk,
  type JobStatus,
} from '@/lib/api'

export const Route = createFileRoute('/jobs/$jobId')({
  component: JobPage,
})

// Mantem so o fim do log em memoria (logs de agentes podem ficar enormes).
const MAX_LOG_CHARS = 200_000

const statusVariant = (status: JobStatus): 'default' | 'secondary' | 'destructive' | 'outline' => {
  if (status === 'COMPLETED') return 'secondary'
  if (status === 'FAILED' || status === 'BLOCKED') return 'destructive'
  if (status === 'WAITING_MERGE' || status === 'WAITING_PR') return 'outline'
  return 'default'
}

function JobPage() {
  const { jobId } = Route.useParams()
  const id = Number(jobId)
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const job = useQuery(jobQueryOptions(id))
  // Eventos: busca incremental com `after=<ultimo id>`, acumulando no cache.
  const eventsKey = ['job', id, 'events'] as const
  const events = useQuery({
    queryKey: eventsKey,
    queryFn: async () => {
      const previous = queryClient.getQueryData<JobEvent[]>(eventsKey) ?? []
      const last = previous.at(-1)?.id ?? 0
      return [...previous, ...(await listEvents(id, last))]
    },
    refetchInterval: (query) => {
      const list = query.state.data ?? []
      if (list.some((e) => e.event === 'JOB_FINISHED') || isTerminal(job.data?.status)) return false
      return 2000
    },
  })

  // Log ao vivo dos agentes: busca incremental por offset de bytes, acumulando no cache.
  const terminal = isTerminal(job.data?.status)
  const logKey = ['job', id, 'log'] as const
  const log = useQuery({
    queryKey: logKey,
    queryFn: async (): Promise<JobLogChunk> => {
      const previous = queryClient.getQueryData<JobLogChunk>(logKey)
      const chunk = await getJobLog(id, previous?.offset ?? 0)
      const text = ((previous?.text ?? '') + chunk.text).slice(-MAX_LOG_CHARS)
      return { text, offset: chunk.offset, size: chunk.size }
    },
    refetchInterval: terminal ? false : 1500,
  })

  // Auto-scroll para o fim, a menos que a pessoa tenha rolado para cima.
  const logRef = useRef<HTMLPreElement>(null)
  const stickToBottom = useRef(true)
  useEffect(() => {
    const el = logRef.current
    if (el && stickToBottom.current) el.scrollTop = el.scrollHeight
  }, [log.data?.text])

  // Job terminou: uma busca final pega eventos e log gravados depois do ultimo poll.
  useEffect(() => {
    if (terminal) {
      void events.refetch()
      void log.refetch()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminal])

  // Acao humana mudou o estado: recarrega este job, o pai (se for item de loop) e a lista.
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ['job'] })
    void queryClient.invalidateQueries({ queryKey: jobsQueryOptions.queryKey })
  }

  const resume = useMutation({
    mutationFn: () => resumeJob(id),
    onSuccess: ({ id: newId }) => {
      refresh()
      // Loop retoma o mesmo job; job comum cria um novo.
      if (newId !== id) void navigate({ to: '/jobs/$jobId', params: { jobId: String(newId) } })
    },
  })
  const approve = useMutation({ mutationFn: () => approveJob(id), onSuccess: refresh })
  const createPr = useMutation({ mutationFn: () => createJobPr(id), onSuccess: refresh })
  const cancel = useMutation({ mutationFn: () => cancelJob(id), onSuccess: refresh })
  const actionError = [resume, approve, createPr, cancel].find((m) => m.isError)?.error

  if (job.isError) {
    return (
      <div className="space-y-4">
        <p className="text-sm text-destructive">{job.error.message}</p>
        <Link to="/runs" className="text-sm underline-offset-4 hover:underline">
          Voltar para jobs
        </Link>
      </div>
    )
  }

  if (job.isLoading || !job.data) {
    return <p className="text-sm text-muted-foreground">Carregando job…</p>
  }

  const data = job.data

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold">
            Job #{data.id}{' '}
            <Badge variant={statusVariant(data.status)} className="ml-2 align-middle">
              {data.status}
            </Badge>
          </h1>
          <p className="text-sm text-muted-foreground">Projeto: {data.project}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {data.pr_url && (
            <a
              href={data.pr_url}
              target="_blank"
              rel="noopener noreferrer"
              className={buttonVariants({ variant: 'outline' })}
            >
              View PR #{data.pr_number} ↗
            </a>
          )}
          {data.actions.create_pr && (
            <Button
              onClick={() => {
                if (window.confirm(`Empurrar a branch ${data.branch} e abrir PR para ${data.base_branch}?`))
                  createPr.mutate()
              }}
              disabled={createPr.isPending}
            >
              {createPr.isPending ? 'Criando PR…' : 'Create PR'}
            </Button>
          )}
          {data.actions.approve && (
            <Button
              onClick={() => {
                if (
                  window.confirm(
                    `Aprovar e fazer MERGE do PR #${data.pr_number} em ${data.base_branch}? Isso e uma acao no GitHub.`,
                  )
                )
                  approve.mutate()
              }}
              disabled={approve.isPending}
            >
              {approve.isPending ? 'Mergeando…' : 'Approve & merge'}
            </Button>
          )}
          {data.actions.cancel && (
            <Button variant="outline" onClick={() => cancel.mutate()} disabled={cancel.isPending}>
              Cancelar
            </Button>
          )}
          {(data.actions.resume || (data.kind === 'job' && terminal)) && (
            <Button variant="outline" onClick={() => resume.mutate()} disabled={resume.isPending}>
              {resume.isPending ? 'Retomando…' : data.kind === 'loop' ? 'Continuar loop' : 'Retomar'}
            </Button>
          )}
        </div>
      </div>

      {actionError && <p className="text-sm text-destructive">{actionError.message}</p>}

      {data.kind === 'work' && (
        <Card>
          <CardHeader>
            <CardDescription>
              Item {data.ref}
              {data.parent_job_id && (
                <>
                  {' '}
                  do{' '}
                  <Link
                    to="/jobs/$jobId"
                    params={{ jobId: String(data.parent_job_id) }}
                    className="underline-offset-4 hover:underline"
                  >
                    loop #{data.parent_job_id}
                  </Link>
                </>
              )}
            </CardDescription>
            <CardTitle className="font-mono text-sm font-normal">
              {data.branch ?? '(branch ainda nao criada)'} → {data.base_branch ?? '?'}
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            <p>
              Review: <Badge variant="outline">{data.review_verdict ?? 'pendente'}</Badge> Merge:{' '}
              <Badge variant="outline">{data.merge_state ?? 'pendente'}</Badge>
            </p>
            {data.review_message && (
              <pre className="whitespace-pre-wrap rounded-md border bg-muted/40 p-3 text-xs">
                {data.review_message}
              </pre>
            )}
          </CardContent>
        </Card>
      )}

      {data.kind === 'loop' && (
        <section className="space-y-2">
          <h2 className="text-lg font-semibold">
            Itens de trabalho <span className="text-sm font-normal text-muted-foreground">(estado: {data.state})</span>
          </h2>
          {data.children.length === 0 ? (
            <p className="text-sm text-muted-foreground">Nenhum item executado ainda.</p>
          ) : (
            <div className="rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Item</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead>Branch → base</TableHead>
                    <TableHead>PR</TableHead>
                    <TableHead>Review</TableHead>
                    <TableHead>Merge</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {data.children.map((child) => (
                    <TableRow key={child.id}>
                      <TableCell>
                        <Link
                          to="/jobs/$jobId"
                          params={{ jobId: String(child.id) }}
                          className="font-medium underline-offset-4 hover:underline"
                        >
                          {child.ref} (#{child.id})
                        </Link>
                      </TableCell>
                      <TableCell>
                        <Badge variant={statusVariant(child.status)}>{child.status}</Badge>
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {child.branch ?? '—'} → {child.base_branch ?? '—'}
                      </TableCell>
                      <TableCell>
                        {child.pr_url ? (
                          <a
                            href={child.pr_url}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="underline-offset-4 hover:underline"
                          >
                            View PR #{child.pr_number} ↗
                          </a>
                        ) : (
                          '—'
                        )}
                      </TableCell>
                      <TableCell>{child.review_verdict ?? '—'}</TableCell>
                      <TableCell>{child.merge_state ?? '—'}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </section>
      )}

      <Card>
        <CardHeader>
          <CardDescription>Prompt</CardDescription>
          <CardTitle className="whitespace-pre-wrap text-base font-normal">{data.prompt}</CardTitle>
        </CardHeader>
      </Card>

      {data.report && (
        <Card>
          <CardHeader>
            <CardDescription>Relatorio</CardDescription>
          </CardHeader>
          <CardContent>
            <pre className="whitespace-pre-wrap text-sm">{data.report}</pre>
          </CardContent>
        </Card>
      )}

      <section className="space-y-2">
        <h2 className="text-lg font-semibold">Tarefas</h2>
        {data.tasks.length === 0 ? (
          <p className="text-sm text-muted-foreground">Nenhuma tarefa ainda.</p>
        ) : (
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Tarefa</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Tentativas</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {data.tasks.map((task) => (
                  <TableRow key={task.id}>
                    <TableCell>{task.title}</TableCell>
                    <TableCell>
                      <Badge variant="outline">{task.status}</Badge>
                    </TableCell>
                    <TableCell>{task.attempts}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </section>

      <section className="space-y-2">
        <h2 className="text-lg font-semibold">Log do agente</h2>
        {log.isError && <p className="text-sm text-destructive">{log.error.message}</p>}
        <pre
          ref={logRef}
          onScroll={(e) => {
            const el = e.currentTarget
            stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
          }}
          className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs"
        >
          {log.data?.text || (terminal ? 'Sem saida registrada para este job.' : 'Aguardando saida do agente…')}
        </pre>
      </section>

      <section className="space-y-2">
        <h2 className="text-lg font-semibold">Eventos</h2>
        {events.isError && <p className="text-sm text-destructive">{events.error.message}</p>}
        <ol className="space-y-1 font-mono text-xs">
          {(events.data ?? []).map((event) => (
            <li key={event.id}>
              <span className="text-muted-foreground">{event.created_at}</span> {event.event}{' '}
              {Object.keys(event.payload).length > 0 && (
                <span className="text-muted-foreground">{JSON.stringify(event.payload)}</span>
              )}
            </li>
          ))}
        </ol>
        {!terminal && <p className="text-xs text-muted-foreground">Acompanhando…</p>}
      </section>
    </div>
  )
}
