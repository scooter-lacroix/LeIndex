import { createRoot } from "react-dom/client";
import App from "./app";
import "./styles.css";

const rootNode = document.getElementById("root");
if (!rootNode) {
  throw new Error("Missing #root mount point");
}

createRoot(rootNode).render(<App />);
