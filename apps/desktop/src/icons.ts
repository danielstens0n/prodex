import {
  ArrowRight01Icon, Cancel01Icon, CommandLineIcon, DragDropVerticalIcon,
  FolderAddIcon, Settings01Icon,
} from "@hugeicons/core-free-icons";

type IconSvgObject = typeof FolderAddIcon;

export const icons = {
  chevron: ArrowRight01Icon, close: Cancel01Icon, terminal: CommandLineIcon,
  drag: DragDropVerticalIcon, addProject: FolderAddIcon, settings: Settings01Icon,
};

// Bundled SVG data keeps icons offline and avoids a framework/runtime dependency.
export function icon(data: IconSvgObject, slot?: string): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  for (const [name, value] of Object.entries({
    viewBox: "0 0 24 24", width: "16", height: "16", fill: "none",
    stroke: "currentColor", "stroke-width": "1.7", "stroke-linecap": "round",
    "stroke-linejoin": "round", "aria-hidden": "true", focusable: "false",
    class: "ui-icon",
  })) svg.setAttribute(name, value);
  if (slot) svg.setAttribute("slot", slot);
  for (const [tag, attributes] of data) {
    const element = document.createElementNS(ns, tag);
    for (const [name, value] of Object.entries(attributes)) {
      if (name === "key" || name === "strokeWidth") continue;
      const attribute = name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`);
      element.setAttribute(attribute, String(value));
    }
    svg.append(element);
  }
  return svg;
}
