import { AlertTriangle, Terminal } from 'lucide-react'
import type { Meta, StoryObj } from '@storybook/react'
import { Alert, AlertDescription, AlertTitle } from './alert'

const meta = {
  title: 'UI/Alert',
  component: Alert,
} satisfies Meta<typeof Alert>

export default meta
type Story = StoryObj<typeof meta>

export const Default: Story = {
  render: () => (
    <Alert className="max-w-md">
      <Terminal />
      <AlertTitle>Command accepted</AlertTitle>
      <AlertDescription>
        The request is queued and will update when processing completes.
      </AlertDescription>
    </Alert>
  ),
}

export const Destructive: Story = {
  render: () => (
    <Alert variant="destructive" className="max-w-md">
      <AlertTriangle />
      <AlertTitle>Request failed</AlertTitle>
      <AlertDescription>
        The operation could not be completed. Review the input and try again.
      </AlertDescription>
    </Alert>
  ),
}
