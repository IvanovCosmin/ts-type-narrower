// Case 27: `this.method()` attribution through the inheritance chain.
// this.speak in Animal and Dog both resolve to Animal.speak; the subclass
// instance call resolves through the chain too. No escapes involved.
class Animal {
  speak(v: "low" | "mid" | "high"): void {
    void v;
  }
  trigger(): void {
    this.speak("low");
  }
}

class Dog extends Animal {
  bark(): void {
    this.speak("mid");
  }
}

const a = new Animal();
const d = new Dog();
a.trigger();
d.bark();
d.speak("mid");
// expected for Animal.speak: unused "high", 3 calls

export {};
