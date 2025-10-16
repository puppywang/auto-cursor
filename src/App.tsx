import { BrowserRouter as Router, Routes, Route } from "react-router-dom";
import { Layout } from "./components/Layout";
import { HomePage } from "./pages/HomePage";
import { MachineIdPage } from "./pages/MachineIdPage";
import { AuthCheckPage } from "./pages/AuthCheckPage";
import { TokenManagePage } from "./pages/TokenManagePage";
import { UsageProvider } from "./context/UsageContext";
import "./App.css";

function App() {
  return (
    <UsageProvider>
      <Router>
        <Layout>
          <Routes>
            <Route path="/" element={<HomePage />} />
            <Route path="/machine-id" element={<MachineIdPage />} />
            <Route path="/auth-check" element={<AuthCheckPage />} />
            <Route path="/token-manage" element={<TokenManagePage />} />
          </Routes>
        </Layout>
      </Router>
    </UsageProvider>
  );
}

export default App;
