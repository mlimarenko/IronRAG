import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-[10px] text-sm font-semibold ring-offset-background transition-all duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0 active:scale-[0.97]",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground shadow-[0_1px_2px_0_hsl(var(--primary)/0.4),inset_0_1px_0_hsl(0_0%_100%/0.15)] hover:shadow-[0_2px_8px_-2px_hsl(var(--primary)/0.5),inset_0_1px_0_hsl(0_0%_100%/0.15)] hover:brightness-110",
        destructive: "bg-destructive text-destructive-foreground shadow-[0_1px_2px_0_hsl(var(--destructive)/0.3),inset_0_1px_0_hsl(0_0%_100%/0.1)] hover:brightness-110",
        outline: "border border-input bg-card shadow-soft hover:bg-accent hover:text-accent-foreground hover:shadow-lifted hover:border-accent",
        secondary: "bg-secondary text-secondary-foreground shadow-soft hover:bg-secondary/80 hover:shadow-lifted",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-10 px-5 py-2",
        sm: "h-9 rounded-[9px] px-4 text-[13px]",
        lg: "h-11 rounded-[11px] px-8",
        icon: "h-10 w-10",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean;
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : "button";
    return <Comp className={cn(buttonVariants({ variant, size, className }))} ref={ref} {...props} />;
  },
);
Button.displayName = "Button";

export { Button, buttonVariants };
