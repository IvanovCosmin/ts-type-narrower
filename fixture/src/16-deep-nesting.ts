// Case 16: five levels of nesting, mixing objects and a discriminated union leaf.
type Leaf =
  | { kind: "a"; a: number }
  | { kind: "b"; b: string }
  | { kind: "c"; c: boolean };

type Deep = {
  l1: {
    l2: {
      l3: {
        l4: {
          leaf: Leaf;
          tag: "x" | "y" | "z";
        };
      };
    };
  };
};

function drill(deep: Deep): void {
  void deep;
}

drill({ l1: { l2: { l3: { l4: { leaf: { kind: "a", a: 1 }, tag: "x" } } } } });
drill({ l1: { l2: { l3: { l4: { leaf: { kind: "b", b: "" }, tag: "y" } } } } });
// expected unused at .l1.l2.l3.l4.leaf: { kind: "c"; c: boolean }
// expected unused at .l1.l2.l3.l4.tag: "z"

export {};
