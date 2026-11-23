import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { jobsQueryOptions } from '@/lib/api'

export const Route = createFileRoute('/')({
  component: Dashboard,
})

function Dashboard() {
  const { data: jobs = [], isLoading, isError, error } = useQuery(jobsQueryOptions)

  const projects = new Set(jobs.map((job) => job.project)).size
  const active = jobs.filter((job) => job.status === 'QUEUED' || job.status === 'RUNNING').length

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold">Dashboard</h1>
        <p className="text-sm text-muted-foreground">Overview of mini-amaru jobs.</p>
      </div>

      {isError && <p className="text-sm text-destructive">{error.message}</p>}

      <div className="grid gap-4 sm:grid-cols-3">
        <StatCard title="Projects" value={isLoading ? '…' : projects} description="Projects with jobs" />
        <StatCard title="Jobs" value={isLoading ? '…' : jobs.length} description="Recent jobs" />
        <StatCard title="Active" value={isLoading ? '…' : active} description="Queued or running" />
      </div>
    </div>
  )
}

function StatCard({
  title,
  value,
  description,
}: {
  title: string
  value: number | string
  description: string
}) {
  return (
    <Card>
      <CardHeader>
        <CardDescription>{title}</CardDescription>
        <CardTitle className="text-3xl">{value}</CardTitle>
      </CardHeader>
      <CardContent>
        <p className="text-xs text-muted-foreground">{description}</p>
      </CardContent>
    </Card>
  )
}
