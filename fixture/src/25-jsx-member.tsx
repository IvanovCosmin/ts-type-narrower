// Case 25: member-expression JSX tags (<UI.Chip/>) are unattributable in the
// MVP and must escape the component, not silently vanish.
const UI = {
  Chip: (p: { tone: "t1" | "t2" | "t3" }): null => {
    void p;
    return null;
  },
};
UI.Chip({ tone: "t1" });
export const el = <UI.Chip tone="t2" />;
// expected: NO finding for Chip
