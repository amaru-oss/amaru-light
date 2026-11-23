import { queryOptions } from '@tanstack/react-query'

// REST API of `cargo run -- -c <config> serve` (default 127.0.0.1:8765).
export const API_BASE: string =
  (import.meta.env.VITE_API_URL as string | undefined) ?? 'http://127.0.0.1:8765'

export type JobStatus =
  | 'QUEUED'
  | 'RUNNING'
  | 'COMPLETED'
  | 'FAILED'
  | 'BLOCKED'
  | 'WAITING_MERGE'
  | 'WAITING_PR'
  | 'STOPPED'
  | 'CANCELLING'
  | 'CANCELLED'
  | 'INTERRUPTED'
  | (string & {})

// "Terminal" = the server is no longer running anything; the job can move again through human action.
export const TERMINAL_STATUSES: JobStatus[] = [
  'COMPLETED',
  'FAILED',
  'BLOCKED',
  'WAITING_MERGE',
  'WAITING_PR',
  'STOPPED',
  'CANCELLED',
  'INTERRUPTED',
]

export function isTerminal(status: JobStatus | undefined): boolean {
  return status !== undefined && TERMINAL_STATUSES.includes(status)
}

export type JobKind = 'job' | 'loop' | 'work'

export type Job = {
  id: number
  project: string
  prompt: string
  status: JobStatus
  kind: JobKind
  parent_job_id: number | null
  ref: string | null
  issue_number: number | null
  branch: string | null
  base_branch: string | null
  pr_url: string | null
  pr_number: number | null
  review_verdict: string | null
  review_message?: string | null
  merge_state: string | null
  state: string | null
  iteration: number
  plan?: string | null
  report?: string | null
  created_at: string
  updated_at: string
}

export type JobActions = { approve: boolean; create_pr: boolean; resume: boolean; cancel: boolean }

export type Task = {
  id: number
  job_id: number
  title: string
  description: string
  task_type: string
  status: string
  attempts: number
  summary: string | null
  error: string | null
}

export type JobDetail = Job & { tasks: Task[]; children: Job[]; actions: JobActions }

export type JobEvent = {
  id: number
  event: string
  payload: Record<string, unknown>
  created_at: string
}

export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response
  try {
    res = await fetch(`${API_BASE}${path}`, {
      ...init,
      headers: { Accept: 'application/json', ...(init?.headers ?? {}) },
    })
  } catch {
    throw new ApiError(`API unavailable at ${API_BASE}. Run: make serve`, 0)
  }
  const body = await res.json().catch(() => null)
  if (!res.ok) {
    const message = (body && typeof body.error === 'string' && body.error) || `HTTP ${res.status}`
    throw new ApiError(message, res.status)
  }
  return body as T
}

export const listJobs = async (): Promise<Job[]> =>
  (await request<{ jobs: Job[] }>('/api/jobs')).jobs

export const getJob = (id: number): Promise<JobDetail> => request<JobDetail>(`/api/jobs/${id}`)

export const listEvents = async (id: number, after: number): Promise<JobEvent[]> =>
  (await request<{ events: JobEvent[] }>(`/api/jobs/${id}/events?after=${after}`)).events

export type JobLogChunk = { text: string; offset: number; size: number }

export const getJobLog = (id: number, offset: number): Promise<JobLogChunk> =>
  request<JobLogChunk>(`/api/jobs/${id}/log?offset=${offset}`)

export type NewJob = { prompt: string; mode: 'job' | 'loop'; milestone?: string }

export const createJob = ({
  prompt,
  mode,
  milestone,
}: NewJob): Promise<{ id: number; status: JobStatus }> =>
  request('/api/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ prompt, mode, ...(milestone?.trim() ? { milestone: milestone.trim() } : {}) }),
  })

// Loop: the SAME job continues (same id). Regular job: creates a new one (resumed_from).
export const resumeJob = (id: number): Promise<{ id: number; resumed?: boolean; resumed_from?: number }> =>
  request(`/api/jobs/${id}/resume`, { method: 'POST' })

export const cancelJob = (id: number): Promise<{ id: number }> =>
  request(`/api/jobs/${id}/cancel`, { method: 'POST' })

// Human actions on a work item's PR.
export const createJobPr = (id: number): Promise<{ id: number }> =>
  request(`/api/jobs/${id}/pr`, { method: 'POST' })

export const approveJob = (
  id: number,
): Promise<{ id: number; status: JobStatus; merge_state: string | null; pr_url: string | null }> =>
  request(`/api/jobs/${id}/approve`, { method: 'POST' })

export const jobsQueryOptions = queryOptions({
  queryKey: ['jobs'],
  queryFn: listJobs,
  refetchInterval: 3000,
})

export const jobQueryOptions = (id: number) =>
  queryOptions({
    queryKey: ['job', id],
    queryFn: () => getJob(id),
    refetchInterval: (query) => (isTerminal(query.state.data?.status) ? false : 2000),
  })
