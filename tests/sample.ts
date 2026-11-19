import type { Shape } from './shape';
import { type Id, helper } from './util';

declare const VERSION: string;

export interface Named { name: string }
export type Pair<A, B> = { a: A; b: B };

namespace Geo {
  export const PI: number = 3.14159;
  export function area(r: number): number { return PI * r * r; }
  export enum Unit { Cm, M }
}

abstract class Base<T> implements Named {
  protected abstract id(): number;
  constructor(public name: string, private readonly tags: string[] = []) {}
  describe(): string { return `${this.name}:${this.tags.length}`; }
}

class Circle extends Base<number> implements Named {
  private r: number;
  constructor(name: string, r: number, public unit: Geo.Unit = Geo.Unit.Cm) {
    super(name, ['round']);
    this.r = r;
  }
  protected id(): number { return 1; }
  area(): number { return Geo.area(this.r); }
}

let a: number = 1, b: string = 'x', c: boolean[] = [true];
const { p, q }: { p: number; q: number } = { p: 1, q: 2 };
function sum(first: number, ...rest: number[]): number { return rest.reduce((x, y) => x + y, first); }
const o = { k: 1 } satisfies Record<string, number>;
const n = (JSON.parse('5') as number)!;
const t = a > 0 ? { v: 1 } : { v: 2 };

const circle = new Circle('c', 2);
console.log(circle.describe(), circle.unit, circle.area().toFixed(2), sum(1, 2, 3), o.k, n, p + q, t.v, a, b, c.length);
