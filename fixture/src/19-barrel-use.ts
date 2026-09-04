import { relay } from "./19-barrel";
import { relay as relay2 } from "./19-barrel-star";
relay("r1");
relay2("r2");
// expected unused: "r3" (calls through both barrel forms are counted)
