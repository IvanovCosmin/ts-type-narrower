// Case 15: JSX usage of a function component counts as a direct call; the
// attributes object is the observed argument.
declare global {
  namespace JSX {
    interface Element {}
    interface ElementChildrenAttribute {
      children: {};
    }
    interface IntrinsicElements {
      [name: string]: unknown;
    }
  }
}

type BadgeProps = {
  variant: "info" | "warn" | "error" | "neutral";
  size: "sm" | "md" | "lg";
};

function Badge(props: BadgeProps): JSX.Element {
  void props;
  return {} as JSX.Element;
}

export const a = <Badge variant="info" size="sm" />;
export const b = <Badge variant="warn" size="sm" />;
// expected unused at .variant: "error", "neutral"
// expected unused at .size: "md", "lg"
