// Case 34: a re-exported namespace object carries the source module's exports;
// calls through it must escape (or better), never vanish.
import { api34 } from "./34-ns-barrel";
api34.nsr("n2");
// expected: NO finding for nsr
