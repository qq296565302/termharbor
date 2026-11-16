/**
 * 跳板机的纯逻辑：哪些连接能当"经由"、列表上怎么显示。
 *
 * 单独抽出来的原因：这里的判定（排除自己、排除成环）是保存前的第一道闸，
 * 后端还会再判一次。放在 lib 里能直接单测，不用起 WebView。
 */

export interface JumpNode {
  id: string;
  name: string;
  /** 它自己的「经由」；`null` = 直连 */
  jumpConnectionId: string | null;
}

/** 走查上限：链本身有三层上限，给到 8 是防脏数据死循环 */
const MAX_HOPS = 8;

/**
 * 能当跳板的候选：排除自己，也排除"经由它最终会绕回自己"的那些。
 *
 * 后端保存时会再判一遍环 —— 这里过滤的目的是让坏选项根本不出现，
 * 而不是等用户点了保存再被拒。
 */
export function jumpCandidates<T extends JumpNode>(nodes: T[], selfId: string | null): T[] {
  // 新建时还没有 id，不存在"绕回自己"的可能，全部可选
  if (!selfId) return nodes;

  const byId = new Map<string, JumpNode>(nodes.map((node) => [node.id, node]));
  return nodes.filter((node) => node.id !== selfId && !reaches(node, selfId, byId));
}

function reaches(start: JumpNode, targetId: string, byId: Map<string, JumpNode>): boolean {
  let cursor: JumpNode | undefined = byId.get(start.id) ?? start;

  for (let hop = 0; hop <= MAX_HOPS; hop += 1) {
    if (!cursor) return false;
    if (cursor.id === targetId) return true;
    cursor = cursor.jumpConnectionId ? byId.get(cursor.jumpConnectionId) : undefined;
  }

  // 链太长（或已成的环）：交给后端报错，别在前端假装算出来了
  return false;
}

/** 列表上显示「经 xxx」用；跳板已被删返回 `null`，不要显示成空字符串。 */
export function jumpName(nodes: JumpNode[], id: string | null | undefined): string | null {
  if (!id) return null;
  return nodes.find((node) => node.id === id)?.name ?? null;
}
