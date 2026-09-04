// Calls the REAL global isNaN; this must not affect 28-global-def's function.
export function useGlobal(v: number): boolean {
  return isNaN(v);
}
useGlobal(1);
