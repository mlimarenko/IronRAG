import { Component, type ReactNode } from "react";
import { AlertTriangle, RotateCw } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/shared/components/ui/alert";
import { Button } from "@/shared/components/ui/button";

type FeatureErrorBoundaryProps = {
  feature: string;
  children: ReactNode;
};

type FeatureErrorBoundaryState = {
  error: unknown;
};

export class FeatureErrorBoundary extends Component<
  FeatureErrorBoundaryProps,
  FeatureErrorBoundaryState
> {
  state: FeatureErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: unknown): FeatureErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: unknown) {
    void import("@/shared/lib/observability").then(({ captureUiException }) =>
      captureUiException(error, { feature: this.props.feature }),
    );
  }

  render() {
    if (this.state.error) {
      return (
        <div className="p-6">
          <Alert variant="destructive" className="mx-auto max-w-2xl">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>{this.props.feature} failed to render</AlertTitle>
            <AlertDescription>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <span>The error was reported. Reload the page to restart this view.</span>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="shrink-0"
                  onClick={() => location.reload()}
                >
                  <RotateCw className="h-3.5 w-3.5" />
                  Reload
                </Button>
              </div>
            </AlertDescription>
          </Alert>
        </div>
      );
    }

    return this.props.children;
  }
}
