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
  const innerRadius = Math.max(10, Math.sqrt(graph.order) * 0.8);
  const rowGap = Math.max(4.5, Math.sqrt(graph.order) * 0.18);
  const arcGap = Math.max(3.2, rowGap * 0.9);

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
  const maxGroupSize = Math.max(...groups.map((group) => group.nodes.length));
  const cell = Math.max(4.2, Math.min(7, 180 / Math.sqrt(graph.order)));
  const rowGap = cell * 1.15;
  const columnGap = cell * 1.7;
  const bandGap = cell * 2.4;
  const baseColumns = Math.max(14, Math.min(72, Math.ceil(Math.sqrt(maxGroupSize) * 3.2)));

  const bandMeasurements = groups.map((group) => {
    const columns = Math.min(baseColumns, Math.max(8, Math.ceil(Math.sqrt(group.nodes.length) * 2.6)));
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
  const typeCount = groups.length;
  const baseRingGap = Math.max(12, Math.sqrt(graph.order) * 0.55);
  const minArcGap = Math.max(2.4, baseRingGap * 0.48);
  let currentRadius = Math.max(baseRingGap * 2, 6);

  groups.forEach((group, ring) => {
    const radiusForCount = (group.nodes.length * minArcGap) / (2 * Math.PI);
    const ringGap =
      ring === 0
        ? baseRingGap * 2
        : baseRingGap * (1 + ring / Math.max(1, typeCount - 1)) + Math.sqrt(currentRadius) * 0.7;

    currentRadius =
      ring === 0
        ? Math.max(baseRingGap * 2.5, radiusForCount)
        : Math.max(radiusForCount, currentRadius + ringGap);

    const angularOffset = ring * (Math.PI / Math.max(6, group.nodes.length));

    for (let index = 0; index < group.nodes.length; index += 1) {
      const angle = (2 * Math.PI * index) / group.nodes.length + angularOffset;
      graph.setNodeAttribute(group.nodes[index], 'x', Math.cos(angle) * currentRadius);
      graph.setNodeAttribute(group.nodes[index], 'y', Math.sin(angle) * currentRadius);
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
