import { cva, type VariantProps } from 'class-variance-authority';

export const buttonVariants = cva(
  'inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all duration-200 outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:pointer-events-none disabled:opacity-50 disabled:transform-none [&_svg]:pointer-events-none [&_svg]:shrink-0',
  {
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground shadow-md shadow-primary/20 hover:-translate-y-0.5 hover:bg-primary/90 hover:shadow-lg hover:shadow-primary/25 active:translate-y-0 active:scale-[.98]',
        outline: 'border border-input bg-background shadow-xs hover:-translate-y-0.5 hover:border-primary/35 hover:bg-accent hover:text-accent-foreground hover:shadow-sm active:translate-y-0 active:scale-[.98]',
        ghost: 'hover:bg-accent hover:text-accent-foreground',
        destructive: 'bg-destructive text-destructive-foreground shadow-sm hover:-translate-y-0.5 hover:bg-destructive/90 hover:shadow-md active:translate-y-0 active:scale-[.98]',
      },
      size: {
        default: 'h-10 px-4 py-2',
        sm: 'h-8 gap-1.5 px-3 text-xs',
        lg: 'h-11 rounded-lg px-5',
        icon: 'size-9',
      },
    },
    defaultVariants: { variant: 'default', size: 'default' },
  },
);

export type ButtonVariants = VariantProps<typeof buttonVariants>;
