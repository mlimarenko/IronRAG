import Graph from 'graphology';
import type { GraphLayoutType } from '@/components/graph/config';

type GroupedNodes = {
  type: string;
  nodes: string[];
};

type PackedCircle = {
  x: number;
  y: number;
  radius: number;
};

function getNodeLabel(graph: Graph, node: string): string {
  const label = graph.getNodeAttribute(node, 'label');
  return typeof label === 'string' ? label : node;
}

function sortNodesByImportance(graph: Graph, nodes: string[]): string[] {
  return [...nodes].sort((left, right) => {
    const degreeDelta = graph.degree(right) - graph.degree(left);
    if (degreeDelta !== 0) return degreeDelta;

    const sizeDelta =
      Number(graph.getNodeAttribute(right, 'size') ?? 0) -
      Number(graph.getNodeAttribute(left, 'size') ?? 0);
    if (sizeDelta !== 0) return sizeDelta;

    return getNodeLabel(graph, left).localeCompare(getNodeLabel(graph, right));
  });
}

function groupNodesByType(graph: Graph): GroupedNodes[] {
  const groups = new Map<string, string[]>();

  graph.forEachNode((node, attrs) => {
    const type = typeof attrs.nodeType === 'string' ? attrs.nodeType : 'entity';
    const nodes = groups.get(type);
    if (nodes) {
      nodes.push(node);
      return;
    }
    groups.set(type, [node]);
  });

  return Array.from(groups.entries())
    .map(([type, nodes]) => ({
      type,
      nodes: sortNodesByImportance(graph, nodes),
    }))
    .sort((left, right) => {
      const sizeDelta = right.nodes.length - left.nodes.length;
      if (sizeDelta !== 0) return sizeDelta;
      return left.type.localeCompare(right.type);
    });
}

/**
 * The minimum visual gap between two adjacent nodes in any layout. All the
 * scale-dependent maths keys off this constant so a tweak here re-tunes
 * every layout consistently.
 */
const NODE_VISUAL_GAP = 6;

/** How many nodes can comfortably sit on a single concentric ring at radius
 *  R, given the global node gap. The ring's circumference is `2πR`, and we
 *  reserve `NODE_VISUAL_GAP` of arc length per node. */
function ringCapacity(radius: number): number {
  return Math.max(8, Math.floor((2 * Math.PI * radius) / NODE_VISUAL_GAP));
}

function layoutNodesInSector(
  graph: Graph,
  nodes: string[],
  startAngle: number,
  endAngle: number,
  innerRadius: number,
  rowGap: number,
  arcGap: number,
): void {
  if (nodes.length === 0) return;

  const sectorPadding = Math.min(0.14, (endAngle - startAngle) * 0.16);
  const usableStart = startAngle + sectorPadding;
  const usableEnd = endAngle - sectorPadding;
  const sectorAngle = Math.max(usableEnd - usableStart, 0.3);

  let index = 0;
  let row = 0;

  while (index < nodes.length) {
    const radius = innerRadius + row * rowGap;
    const capacity = Math.max(1, Math.floor((sectorAngle * Math.max(radius, innerRadius)) / arcGap));
    const count = Math.min(capacity, nodes.length - index);

    for (let offset = 0; offset < count; offset += 1) {
      const node = nodes[index + offset];
      const ratio = count === 1 ? 0.5 : offset / (count - 1);
      const angle = usableStart + sectorAngle * ratio;

      graph.setNodeAttribute(node, 'x', Math.cos(angle) * radius);
      graph.setNodeAttribute(node, 'y', Math.sin(angle) * radius);
    }

    index += count;
    row += 1;
  }
}

function layoutSectors(graph: Graph): void {
  if (graph.order === 0) return;

  const groups = groupNodesByType(graph);
  const sectorGap = Math.min(0.18, (2 * Math.PI) / Math.max(18, groups.length * 3));
  const usableAngle = 2 * Math.PI - groups.length * sectorGap;
  const weights = groups.map((group) => Math.sqrt(group.nodes.length + 2));
  const totalWeight = weights.reduce((sum, weight) => sum + weight, 0);
  // Spacing GROWS with graph size aggressively so Sigma's autoRescale
  // (which fits the entire layout into the viewport) still leaves enough
  // pixels between node centers for them to be visually distinct on dense
  // datasets. Previous factors collapsed at 5k+ nodes after rescale.
  const orderRoot = Math.sqrt(graph.order);
  const innerRadius = Math.max(40, orderRoot * 4);
  const rowGap = Math.max(NODE_VISUAL_GAP * 2.5, orderRoot * 1.4);
  const arcGap = Math.max(NODE_VISUAL_GAP * 2, rowGap);

  let cursor = -Math.PI / 2;

  groups.forEach((group, index) => {
    const sectorAngle = usableAngle * (weights[index] / totalWeight);
    const startAngle = cursor;
    const endAngle = cursor + sectorAngle;
    layoutNodesInSector(graph, group.nodes, startAngle, endAngle, innerRadius, rowGap, arcGap);
    cursor = endAngle + sectorGap;
  });
}

function layoutBands(graph: Graph): void {
  if (graph.order === 0) return;

  const groups = groupNodesByType(graph);
  // `cell` is the base unit of separation between adjacent nodes. We push
  // it aggressively higher than before so that even after Sigma's
  // `autoRescale` shrinks 25k+ nodes down to fit the viewport, the
  // resulting per-cell pixel size is still wide enough to keep nodes
  // visually distinct. The previous `orderRoot * 0.32` capped cell at
  // ~50 for 25k nodes; once Sigma compressed that to fit a 600 px canvas
  // every cell collapsed to ~3 px and the entire graph painted as one
  // colored block. The new factor (1.4) reserves enough layout-space
  // headroom that even the worst rescaling still leaves ~6-8 px between
  // node centers at 25k nodes.
  const orderRoot = Math.sqrt(graph.order);
  const cell = Math.max(NODE_VISUAL_GAP * 2, orderRoot * 1.4);
  const rowGap = cell * 1.5;
  const columnGap = cell * 2;
  const bandGap = cell * 4;

  // Width of each band is bounded so that even the largest group does not
  // become an absurdly wide horizontal smear; instead it wraps onto more
  // rows. Aspect ratio stays close to "comfortable strip", not "string".
  const maxColumns = Math.max(20, Math.min(96, Math.ceil(orderRoot * 1.6)));

  const bandMeasurements = groups.map((group) => {
    const columns = Math.min(
      maxColumns,
      Math.max(10, Math.ceil(Math.sqrt(group.nodes.length) * 2.4)),
    );
    const rows = Math.max(1, Math.ceil(group.nodes.length / columns));
    const bandHeight = rows * rowGap;
    return { group, columns, rows, bandHeight };
  });

  const totalHeight =
    bandMeasurements.reduce((sum, band) => sum + band.bandHeight, 0) +
    Math.max(0, bandMeasurements.length - 1) * bandGap;

  let currentY = -totalHeight / 2;

  bandMeasurements.forEach(({ group, columns, bandHeight }) => {
    for (let index = 0; index < group.nodes.length; index += 1) {
      const node = group.nodes[index];
      const row = Math.floor(index / columns);
      const column = index % columns;
      const rowStart = row * columns;
      const rowCount = Math.min(columns, group.nodes.length - rowStart);
      const rowWidth = Math.max(1, rowCount - 1) * columnGap;
      const x =
        rowCount === 1
          ? 0
          : column * columnGap - rowWidth / 2 + (row % 2 === 1 ? columnGap * 0.15 : 0);
      const rowOffset = row * rowGap;
      const y = currentY + rowOffset + bandHeight / 2 - cell / 2;

      graph.setNodeAttribute(node, 'x', x);
      graph.setNodeAttribute(node, 'y', y);
    }

    currentY += bandHeight + bandGap;
  });
}

function layoutRings(graph: Graph): void {
  if (graph.order === 0) return;

  const groups = groupNodesByType(graph);

  // Each concentric ring is placed at a uniform additive gap from the
  // previous one. The gap GROWS with graph size so that on dense graphs,
  // after Sigma's autoRescale fits the layout into the viewport, the
  // physical pixel distance between rings is still large enough for the
  // rings to be visually distinct discs of nodes.
  const orderRoot = Math.sqrt(graph.order);
  const ringGap = Math.max(60, orderRoot * 1.6);
  const innerRadius = Math.max(80, orderRoot * 2.2);

  // Big groups (e.g. 4000 entities) cannot fit on a single ring without
  // overlapping. Split them across multiple sub-rings so each sub-ring
  // stays at a comfortable density. The capacity of a ring grows with its
  // radius (more circumference = more nodes), so we figure out the radius
  // first, then partition the group into chunks small enough to fit.
  type RingPlan = { type: string; nodes: string[]; radius: number };
  const ringPlans: RingPlan[] = [];

  let currentRadius = innerRadius;
  groups.forEach((group) => {
    let remaining = group.nodes.length;
    let pointer = 0;
    while (remaining > 0) {
      const capacity = ringCapacity(currentRadius);
      const taken = Math.min(remaining, capacity);
      ringPlans.push({
        type: group.type,
        nodes: group.nodes.slice(pointer, pointer + taken),
        radius: currentRadius,
      });
      pointer += taken;
      remaining -= taken;
      currentRadius += ringGap;
    }
  });

  ringPlans.forEach((plan, ringIndex) => {
    // Stagger the angular start position per ring so neighbouring sub-rings
    // do not align their nodes on the same radial spoke (looks like spokes
    // instead of concentric circles).
    const angularOffset = ringIndex * (Math.PI / 6);
    const count = plan.nodes.length;
    if (count === 0) return;
    const step = (2 * Math.PI) / count;
    for (let index = 0; index < count; index += 1) {
      const angle = angularOffset + index * step;
      graph.setNodeAttribute(plan.nodes[index], 'x', Math.cos(angle) * plan.radius);
      graph.setNodeAttribute(plan.nodes[index], 'y', Math.sin(angle) * plan.radius);
    }
  });
}

function layoutClusters(graph: Graph): void {
  if (graph.order === 0) return;

  const groups = groupNodesByType(graph);
  const orbitRadius = Math.sqrt(graph.order) * 8;
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));

  groups.forEach((group, clusterIndex) => {
    const centroidAngle = (2 * Math.PI * clusterIndex) / groups.length;
    const centerX = Math.cos(centroidAngle) * orbitRadius;
    const centerY = Math.sin(centroidAngle) * orbitRadius;
    const clusterRadius = Math.sqrt(group.nodes.length) * 2.5;

    for (let index = 0; index < group.nodes.length; index += 1) {
      const distance = clusterRadius * Math.sqrt((index + 0.5) / group.nodes.length);
      const angle = index * goldenAngle;
      graph.setNodeAttribute(group.nodes[index], 'x', centerX + Math.cos(angle) * distance);
      graph.setNodeAttribute(group.nodes[index], 'y', centerY + Math.sin(angle) * distance);
    }
  });
}

function findConnectedComponents(graph: Graph): string[][] {
  const visited = new Set<string>();
  const components: string[][] = [];

  graph.forEachNode((node) => {
    if (visited.has(node)) return;

    const queue = [node];
    const component: string[] = [];
    visited.add(node);

    while (queue.length > 0) {
      const current = queue.shift();
      if (!current) continue;

      component.push(current);

      graph.forEachNeighbor(current, (neighbor) => {
        if (visited.has(neighbor)) return;
        visited.add(neighbor);
        queue.push(neighbor);
      });
    }

    components.push(component);
  });

  return components.sort((left, right) => right.length - left.length);
}

function componentRadius(size: number): number {
  if (size <= 1) return 3;
  if (size === 2) return 7;
  return Math.max(11, Math.sqrt(size) * 5.6);
}

function packComponentCircle(placed: PackedCircle[], radius: number): PackedCircle {
  if (placed.length === 0) {
    return { x: 0, y: 0, radius };
  }

  const gap = 12;
  for (let step = 0; step < 3200; step += 1) {
    const angle = step * 0.58;
    const distance = 14 + step * 0.92;
    const x = Math.cos(angle) * distance;
    const y = Math.sin(angle) * distance;

    const overlaps = placed.some((circle) => {
      const minDistance = circle.radius + radius + gap;
      return Math.hypot(x - circle.x, y - circle.y) < minDistance;
    });

    if (!overlaps) {
      return { x, y, radius };
    }
  }

  const fallbackOffset = placed.length * (radius + gap);
  return { x: fallbackOffset, y: fallbackOffset * 0.15, radius };
}

function layoutComponentNodes(graph: Graph, nodes: string[], centerX: number, centerY: number, radius: number): void {
  if (nodes.length === 1) {
    graph.setNodeAttribute(nodes[0], 'x', centerX);
    graph.setNodeAttribute(nodes[0], 'y', centerY);
    return;
  }

  if (nodes.length === 2) {
    graph.setNodeAttribute(nodes[0], 'x', centerX - radius * 0.35);
    graph.setNodeAttribute(nodes[0], 'y', centerY);
    graph.setNodeAttribute(nodes[1], 'x', centerX + radius * 0.35);
    graph.setNodeAttribute(nodes[1], 'y', centerY);
    return;
  }

  const sorted = sortNodesByImportance(graph, nodes);
  const usableRadius = Math.max(4, radius - 4);
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));

  for (let index = 0; index < sorted.length; index += 1) {
    const node = sorted[index];
    const ratio = Math.sqrt((index + 0.5) / sorted.length);
    const distance = usableRadius * ratio;
    const angle = index * goldenAngle;

    graph.setNodeAttribute(node, 'x', centerX + Math.cos(angle) * distance);
    graph.setNodeAttribute(node, 'y', centerY + Math.sin(angle) * distance);
  }
}

function layoutComponents(graph: Graph): void {
  if (graph.order === 0) return;

  const components = findConnectedComponents(graph).map((nodes) => ({
    nodes: sortNodesByImportance(graph, nodes),
    radius: componentRadius(nodes.length),
  }));

  const packedCircles: PackedCircle[] = [];

  components.forEach((component) => {
    const packed = packComponentCircle(packedCircles, component.radius);
    packedCircles.push(packed);
    layoutComponentNodes(graph, component.nodes, packed.x, packed.y, packed.radius);
  });
}

export function applyGraphLayout(graph: Graph, layout: GraphLayoutType): void {
  switch (layout) {
    case 'sectors':
      layoutSectors(graph);
      return;
    case 'bands':
      layoutBands(graph);
      return;
    case 'components':
      layoutComponents(graph);
      return;
    case 'rings':
      layoutRings(graph);
      return;
    case 'clusters':
      layoutClusters(graph);
      return;
  }
}
