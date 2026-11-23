import { useQuery } from '@tanstack/react-query'
import { createFileRoute, Link } from '@tanstack/react-router'
import { flexRender, getCoreRowModel, useReactTable, type ColumnDef } from '@tanstack/react-table'
import { Badge } from '@/components/ui/badge'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { jobsQueryOptions, type Job, type JobStatus } from '@/lib/api'

export const Route = createFileRoute('/runs')({
  component: RunsPage,
})

const statusVariant = (status: JobStatus): 'default' | 'secondary' | 'destructive' | 'outline' => {
  if (status === 'COMPLETED') return 'secondary'
  if (status === 'FAILED' || status === 'BLOCKED') return 'destructive'
  if (status === 'WAITING_MERGE' || status === 'WAITING_PR') return 'outline'
  return 'default'
}

const columns: ColumnDef<Job>[] = [
  {
    accessorKey: 'id',
    header: 'ID',
    cell: ({ row }) => (
      <Link
        to="/jobs/$jobId"
        params={{ jobId: String(row.original.id) }}
        className="font-medium underline-offset-4 hover:underline"
      >
        #{row.original.id}
      </Link>
    ),
  },
  {
    accessorKey: 'kind',
    header: 'Type',
    cell: ({ row }) => (
      <Badge variant="outline">
        {row.original.kind === 'work'
          ? `item ${row.original.ref ?? ''} (loop #${row.original.parent_job_id})`
          : row.original.kind}
      </Badge>
    ),
  },
  {
    accessorKey: 'prompt',
    header: 'Prompt',
    cell: ({ getValue }) => <span className="line-clamp-1 max-w-md">{getValue<string>()}</span>,
  },
  {
    accessorKey: 'status',
    header: 'Status',
    cell: ({ getValue }) => {
      const status = getValue<JobStatus>()
      return <Badge variant={statusVariant(status)}>{status}</Badge>
    },
  },
  {
    accessorKey: 'pr_url',
    header: 'PR',
    cell: ({ row }) =>
      row.original.pr_url ? (
        <a
          href={row.original.pr_url}
          target="_blank"
          rel="noopener noreferrer"
          className="underline-offset-4 hover:underline"
        >
          #{row.original.pr_number}
        </a>
      ) : (
        '—'
      ),
  },
  { accessorKey: 'created_at', header: 'Created at' },
]

function RunsPage() {
  const { data = [], isLoading, isError, error } = useQuery(jobsQueryOptions)

  const table = useReactTable({
    data,
    columns,
    getCoreRowModel: getCoreRowModel(),
  })

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">Jobs</h1>
        <Link to="/new" className="text-sm font-medium underline-offset-4 hover:underline">
          New job
        </Link>
      </div>

      {isLoading && <p className="text-sm text-muted-foreground">Loading jobs…</p>}
      {isError && <p className="text-sm text-destructive">{error.message}</p>}

      {!isLoading && !isError && data.length === 0 && (
        <p className="text-sm text-muted-foreground">No jobs yet.</p>
      )}

      {!isLoading && !isError && data.length > 0 && (
        <div className="rounded-md border">
          <Table>
            <TableHeader>
              {table.getHeaderGroups().map((headerGroup) => (
                <TableRow key={headerGroup.id}>
                  {headerGroup.headers.map((header) => (
                    <TableHead key={header.id}>
                      {flexRender(header.column.columnDef.header, header.getContext())}
                    </TableHead>
                  ))}
                </TableRow>
              ))}
            </TableHeader>
            <TableBody>
              {table.getRowModel().rows.map((row) => (
                <TableRow key={row.id}>
                  {row.getVisibleCells().map((cell) => (
                    <TableCell key={cell.id}>
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </TableCell>
                  ))}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  )
}
