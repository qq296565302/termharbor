import { describe, expect, it } from "vitest";

import { jumpCandidates, jumpName, type JumpNode } from "./jump";

function node(id: string, jumpConnectionId: string | null = null): JumpNode {
  return { id, name: `连接-${id}`, jumpConnectionId };
}

describe("jumpCandidates", () => {
  it("新建时（还没有 id）全部可选", () => {
    const all = [node("a"), node("b")];
    expect(jumpCandidates(all, null)).toHaveLength(2);
  });

  it("排除自己", () => {
    const all = [node("a"), node("b")];
    expect(jumpCandidates(all, "a").map((n) => n.id)).toEqual(["b"]);
  });

  it("排除直接绕回自己的那条", () => {
    // b 已经经由 a，所以 a 不能再经由 b
    const all = [node("a"), node("b", "a"), node("c")];
    expect(jumpCandidates(all, "a").map((n) => n.id)).toEqual(["c"]);
  });

  it("排除隔一层才绕回来的", () => {
    // c → b → a：a 选 c 会成三节点环
    const all = [node("a"), node("b", "a"), node("c", "b")];
    expect(jumpCandidates(all, "a").map((n) => n.id)).toEqual([]);
  });

  it("不相关的一律保留", () => {
    const all = [node("a"), node("b", "x"), node("c", "b")];
    expect(jumpCandidates(all, "a").map((n) => n.id)).toEqual(["b", "c"]);
  });

  it("已断链（跳板指向不存在的 id）不会崩也不会误排除", () => {
    const all = [node("a"), node("b", "ghost")];
    expect(jumpCandidates(all, "a").map((n) => n.id)).toEqual(["b"]);
  });
});

describe("jumpName", () => {
  it("按 id 找到名字", () => {
    expect(jumpName([node("a"), node("b")], "b")).toBe("连接-b");
  });

  it("空值与已删除的跳板都返回 null", () => {
    expect(jumpName([node("a")], null)).toBeNull();
    expect(jumpName([node("a")], "gone")).toBeNull();
  });
});
