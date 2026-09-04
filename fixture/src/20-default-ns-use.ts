import D from "./20-default-def";
import * as api from "./20-ns-def";
D("d1");
api.nsfn("n1");
api.nsfn("n2");
// expected: dflt unused "d2","d3"; nsfn unused "n3"
