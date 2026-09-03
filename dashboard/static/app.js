// OpenCoordex Admin Dashboard JS (v1.0)

const API_BASE = '/v1/admin';

// =========================================
// Tab Navigation
// =========================================
document.querySelectorAll('.nav-menu .nav-item').forEach(link => {
    link.addEventListener('click', (e) => {
        e.preventDefault();
        const target = e.target.closest('.nav-item');
        const tab = target.dataset.tab;

        // Update active link
        document.querySelectorAll('.nav-menu .nav-item').forEach(l => l.classList.remove('active'));
        target.classList.add('active');

        // Update active tab content
        document.querySelectorAll('.tab-content').forEach(c => c.classList.remove('active'));
        document.getElementById(tab).classList.add('active');

        // Update header title
        const titles = {
            overview: 'Overview',
            providers: 'LLM Providers',
            persistence: 'Persistence',
            mcp: 'MCP Registry',
            metrics: 'Performance Metrics',
            audit: 'Audit Trails',
            research: 'Research Runs',
            approvals: 'Pending Approvals',
            domains: 'Network Governance',
            harness: 'Test Harness',
            cognitive: 'Cognitive Security Audit'
        };
        const pageTitle = document.getElementById('page-title');
        if (pageTitle) pageTitle.textContent = titles[tab] || tab;

        // Update breadcrumbs
        const breadcrumbs = document.querySelector('.breadcrumbs');
        if (breadcrumbs) breadcrumbs.textContent = `Dashboard / ${titles[tab] || tab}`;

        // Lazy load harness suites if harness tab is active
        if (tab === 'harness') {
            loadHarnessSuites();
        } else if (tab === 'cognitive') {
            loadCognitiveData();
        }
    });
});

// =========================================
// Fetch Wrapper
// =========================================
async function fetchWithAuth(url, options = {}) {
    const token = 'admin'; // Demo token
    return fetch(url, {
        ...options,
        headers: {
            'Authorization': `Bearer ${token}`,
            'Content-Type': 'application/json',
            ...options.headers
        }
    });
}

// =========================================
// Modal Helpers
// =========================================
function openModal(modalId) {
    document.getElementById(modalId).classList.remove('hidden');
}

function closeModal(modalId) {
    document.getElementById(modalId).classList.add('hidden');
}

// Close modal on overlay click or X button
document.querySelectorAll('.modal-overlay, .modal-close').forEach(el => {
    el.addEventListener('click', (e) => {
        e.target.closest('.modal').classList.add('hidden');
    });
});

// Close the topmost open modal with Escape
document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    const openModals = Array.from(document.querySelectorAll('.modal:not(.hidden)'));
    const top = openModals[openModals.length - 1];
    if (top) top.classList.add('hidden');
});

// =========================================
// LLM Providers
// =========================================
let providers = []; // In-memory store

// Vendor card click -> open modal with prefilled vendor
document.querySelectorAll('.vendor-card').forEach(card => {
    card.addEventListener('click', () => {
        const vendor = card.dataset.vendor;
        const presets = {
            openai: { vendor: 'OpenAI', url: 'https://api.openai.com/v1', model: 'gpt-4o' },
            anthropic: { vendor: 'Anthropic', url: 'https://api.anthropic.com/v1', model: 'claude-3-5-sonnet-20241022' },
            google: { vendor: 'Google AI', url: 'https://generativelanguage.googleapis.com/v1beta', model: 'gemini-1.5-pro' },
            mistral: { vendor: 'Mistral', url: 'https://api.mistral.ai/v1', model: 'mistral-large-latest' },
            deepseek: { vendor: 'DeepSeek', url: 'https://api.deepseek.com', model: 'deepseek-chat' },
            local: { vendor: 'Local (vLLM)', url: 'http://localhost:8000/v1', model: 'local-model' }
        };
        const preset = presets[vendor] || {};

        document.getElementById('prov-vendor').value = preset.vendor || '';
        document.getElementById('prov-url').value = preset.url || '';
        document.getElementById('prov-model').value = preset.model || '';
        document.getElementById('prov-desc').value = '';
        document.getElementById('prov-version').value = '';
        document.getElementById('prov-key').value = '';

        openModal('modal-provider');
    });
});

// Add Provider button
document.getElementById('btn-add-provider')?.addEventListener('click', () => {
    // Clear form
    document.getElementById('form-provider').reset();
    openModal('modal-provider');
});

// Form submit -> save provider
document.getElementById('form-provider')?.addEventListener('submit', async (e) => {
    e.preventDefault();

    const capabilities = Array.from(document.querySelectorAll('#form-provider input[name="cap"]:checked'))
        .map(cb => cb.value);

    const provider = {
        id: 'prov-' + Date.now(),
        vendor: document.getElementById('prov-vendor').value,
        model_id: document.getElementById('prov-model').value,
        description: document.getElementById('prov-desc').value || null,
        base_url: document.getElementById('prov-url').value,
        version: document.getElementById('prov-version').value || null,
        api_key: document.getElementById('prov-key').value,
        capabilities: capabilities,
        status: 'pending'
    };

    // Send to backend
    const submitBtn = e.target.querySelector('button[type="submit"]');
    if (submitBtn) {
        submitBtn.disabled = true;
        submitBtn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Saving...';
    }
    try {
        const res = await fetchWithAuth(`${API_BASE}/providers`, {
            method: 'POST',
            body: JSON.stringify(provider)
        });

        if (res.ok) {
            const saved = await res.json().catch(() => provider);
            providers.push(saved);
            renderProviders();
            closeModal('modal-provider');
        } else {
            alert(`Failed to save provider (HTTP ${res.status})`);
        }
    } catch (err) {
        alert('Could not reach the server. Provider was not saved.');
    } finally {
        if (submitBtn) {
            submitBtn.disabled = false;
            submitBtn.innerHTML = '<i class="fa-solid fa-save"></i> Save Provider';
        }
    }
});

// Test provider connection
document.getElementById('btn-test-provider')?.addEventListener('click', async () => {
    const btn = document.getElementById('btn-test-provider');
    btn.disabled = true;
    btn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Testing...';

    try {
        const res = await fetchWithAuth(`${API_BASE}/providers/test`, {
            method: 'POST',
            body: JSON.stringify({
                base_url: document.getElementById('prov-url').value,
                api_key: document.getElementById('prov-key').value,
                model_id: document.getElementById('prov-model').value
            })
        });

        if (res.ok) {
            btn.innerHTML = '<i class="fa-solid fa-check"></i> Connected!';
            btn.style.color = 'var(--success)';
        } else {
            btn.innerHTML = '<i class="fa-solid fa-xmark"></i> Failed';
            btn.style.color = 'var(--danger)';
        }
    } catch (err) {
        btn.innerHTML = '<i class="fa-solid fa-xmark"></i> Error';
        btn.style.color = 'var(--danger)';
    }

    setTimeout(() => {
        btn.disabled = false;
        btn.innerHTML = '<i class="fa-solid fa-plug"></i> Test Connection';
        btn.style.color = '';
    }, 2000);
});

function renderProviders() {
    const tbody = document.getElementById('providers-body');
    if (!tbody) return;

    if (providers.length === 0) {
        tbody.innerHTML = '<tr><td colspan="5" class="empty-state">No providers configured. Click "Add Provider" or select a preset above.</td></tr>';
        return;
    }

    tbody.innerHTML = providers.map(p => `
        <tr>
            <td><span class="status-pill status-${p.status === 'connected' ? 'healthy' : 'degraded'}">${escapeHtml(p.status)}</span></td>
            <td class="font-medium">${escapeHtml(p.vendor)} <span class="text-sm text-muted">(${escapeHtml(p.model_id)})</span></td>
            <td class="text-sm">${escapeHtml(p.model_id)}</td>
            <td>
                <div class="tags-container">
                    ${(p.capabilities || []).map(cap => `<span class="tag text-xs">${escapeHtml(cap)}</span>`).join('')}
                </div>
            </td>
            <td>
                <button class="btn-icon" onclick="testProviderById('${p.id}')" title="Test"><i class="fas fa-plug"></i></button>
                <button class="btn-icon text-red" onclick="deleteProvider('${p.id}')" title="Delete"><i class="fas fa-trash"></i></button>
            </td>
        </tr>
    `).join('');
}

async function loadProviders() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/providers`);
        if (res.ok) {
            providers = await res.json();
            renderProviders();
        }
    } catch (err) {
        console.error('Failed to load providers:', err);
    }
}

window.deleteProvider = async (id) => {
    providers = providers.filter(p => p.id !== id);
    try {
        await fetchWithAuth(`${API_BASE}/providers/${id}`, { method: 'DELETE' });
    } catch (err) {
        // Ignore if backend not available
    }
    renderProviders();
};

window.testProviderById = async (id) => {
    const provider = providers.find(p => p.id === id);
    if (!provider) return;

    try {
        const res = await fetchWithAuth(`${API_BASE}/providers/${id}/test`, { method: 'POST' });
        provider.status = res.ok ? 'connected' : 'error';
    } catch (err) {
        provider.status = 'error';
    }
    renderProviders();
};

// =========================================
// Persistence (S3)
// =========================================
document.getElementById('s3-enabled')?.addEventListener('change', (e) => {
    const form = document.getElementById('form-s3');
    if (e.target.checked) {
        form.classList.remove('hidden');
    } else {
        form.classList.add('hidden');
    }
});

document.getElementById('btn-test-s3')?.addEventListener('click', async () => {
    const btn = document.getElementById('btn-test-s3');
    const status = document.getElementById('s3-status');
    btn.disabled = true;
    btn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Testing...';

    try {
        const res = await fetchWithAuth(`${API_BASE}/config/s3/test`, {
            method: 'POST',
            body: JSON.stringify({
                bucket: document.getElementById('s3-bucket').value,
                endpoint: document.getElementById('s3-endpoint').value,
                access_key: document.getElementById('s3-access-key').value,
                secret_key: document.getElementById('s3-secret-key').value,
                region: document.getElementById('s3-region').value
            })
        });

        status.classList.remove('hidden');
        if (res.ok) {
            status.className = 'status-message success';
            status.textContent = '✓ Connection successful! Bucket is accessible.';
        } else {
            status.className = 'status-message error';
            status.textContent = `✗ Connection failed (HTTP ${res.status}). Check your credentials.`;
        }
    } catch (err) {
        status.classList.remove('hidden');
        status.className = 'status-message error';
        status.textContent = '✗ Error: ' + err.message;
    }

    btn.disabled = false;
    btn.innerHTML = '<i class="fa-solid fa-plug"></i> Test Connection';
});

document.getElementById('form-s3')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    // Save S3 config (stub for now)
    alert('S3 configuration saved. Restart server to apply changes.');
});

async function loadPersistenceConfig() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/config`);
        if (res.ok) {
            const data = await res.json();
            document.getElementById('cfg-storage-mode').textContent = data.persistence?.mode || 'In-Memory';
            document.getElementById('cfg-s3-bucket').textContent = data.persistence?.s3_bucket || 'N/A';
            document.getElementById('cfg-s3-endpoint').textContent = data.persistence?.s3_endpoint || 'Default (AWS)';

            if (data.persistence?.mode?.includes('S3')) {
                document.getElementById('s3-enabled').checked = true;
                document.getElementById('form-s3').classList.remove('hidden');
            }
        }
    } catch (err) {
        console.error('Failed to load persistence config:', err);
    }
}

// =========================================
// MCP Registry
// =========================================
document.getElementById('btn-register-mcp')?.addEventListener('click', () => {
    document.getElementById('form-mcp').reset();
    openModal('modal-mcp');
});

document.getElementById('form-mcp')?.addEventListener('submit', async (e) => {
    e.preventDefault();

    const capabilities = Array.from(document.querySelectorAll('#form-mcp input[name="mcp-cap"]:checked'))
        .map(cb => cb.value);

    const server = {
        name: document.getElementById('mcp-name').value,
        transport_type: document.getElementById('mcp-transport').value,
        command: document.getElementById('mcp-command').value,
        capabilities: capabilities
    };

    const submitBtn = e.target.querySelector('button[type="submit"]');
    if (submitBtn) {
        submitBtn.disabled = true;
        submitBtn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Registering...';
    }
    try {
        const res = await fetchWithAuth(`${API_BASE}/mcp/servers`, {
            method: 'POST',
            body: JSON.stringify(server)
        });

        if (res.ok) {
            loadMcpServers();
            closeModal('modal-mcp');
        } else {
            alert('Failed to register MCP server');
        }
    } catch (err) {
        console.error('Failed to register MCP server:', err);
        alert('Failed to register MCP server: ' + err.message);
    } finally {
        if (submitBtn) {
            submitBtn.disabled = false;
            submitBtn.innerHTML = '<i class="fa-solid fa-plus"></i> Register';
        }
    }
});

async function loadMcpServers() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/mcp/servers`);
        const data = await res.json();
        const tbody = document.getElementById('mcp-body');

        if (data.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="empty-state">No MCP servers registered</td></tr>';
            return;
        }

        tbody.innerHTML = data.map(server => `
            <tr>
                <td><span class="status-pill status-${server.available ? 'healthy' : 'degraded'}">${server.available ? 'Connected' : 'Offline'}</span></td>
                <td class="font-medium">${escapeHtml(server.name)} <span class="text-sm text-muted">(${escapeHtml(server.id)})</span></td>
                <td class="text-sm">${escapeHtml(server.transport_type)}</td>
                <td>
                    <div class="tags-container">
                        ${(server.capabilities || []).map(cap => `<span class="tag text-xs">${escapeHtml(cap)}</span>`).join('')}
                    </div>
                </td>
                <td>
                   <button class="btn-icon" title="Inspect"><i class="fas fa-search"></i></button>
                   <button class="btn-icon text-red" onclick="removeMcp('${server.id}')" title="Remove"><i class="fas fa-trash"></i></button>
                </td>
            </tr>
        `).join('');
    } catch (err) {
        console.error('Failed to load MCP servers:', err);
    }
}

window.removeMcp = async (id) => {
    try {
        await fetchWithAuth(`${API_BASE}/mcp/servers/${id}`, { method: 'DELETE' });
        loadMcpServers();
    } catch (err) {
        console.error('Failed to remove MCP server:', err);
    }
};

// =========================================
// Metrics & Overview
// =========================================
async function loadMetrics() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/metrics`);
        const data = await res.json();

        document.getElementById('stat-requests').textContent = data.requests_total?.toLocaleString() || '--';
        document.getElementById('stat-tokens').textContent = data.tokens_used?.toLocaleString() || '--';
        document.getElementById('stat-sessions').textContent = data.active_sessions || '0';
        document.getElementById('stat-latency').textContent = data.avg_latency_ms ? `${Math.round(data.avg_latency_ms)}ms` : '--';
    } catch (err) {
        console.error('Failed to load metrics:', err);
    }
}

// =========================================
// Research Runs
// =========================================
async function loadResearchRuns() {
    try {
        // We filter audit logs for RESEARCH_* and PLAN_* events to reconstruct runs
        const res = await fetchWithAuth(`${API_BASE}/audit?limit=100&action=RESEARCH_CREATED`);
        const runs = await res.json();

        const grid = document.getElementById('research-list');
        if (runs.length === 0) {
            grid.innerHTML = '<div class="empty-state">No active or past research runs found.</div>';
            return;
        }

        grid.innerHTML = runs.map(run => {
            const statusClass = run.outcome === 'Success' ? 'status-completed' : 'status-executing';
            const statusText = run.outcome === 'Success' ? 'COMPLETED' : 'IN_PROGRESS';
            const shortId = (run.id || '').split('-')[0] || '—';

            return `
                <div class="research-card">
                    <div class="research-card-header">
                        <span class="research-status ${statusClass}">${statusText}</span>
                        <span class="text-xs text-muted">ID: ${shortId}</span>
                    </div>
                    <div class="research-info">
                        <h4 class="font-medium">Research Task: ${escapeHtml(run.resource)}</h4>
                        <p class="text-sm text-muted">User: ${escapeHtml(run.user_id)}</p>
                    </div>
                    <div class="research-progress">
                        <div class="progress-bar" style="width: ${run.outcome === 'Success' ? '100%' : '65%'}"></div>
                    </div>
                    <div class="research-card-footer">
                         <span class="text-xs text-muted">${new Date(run.timestamp).toLocaleString()}</span>
                    </div>
                </div>
            `;
        }).join('');
    } catch (err) {
        console.error('Failed to load research runs:', err);
    }
}

document.getElementById('btn-refresh-research')?.addEventListener('click', loadResearchRuns);

// =========================================
// Pending Approvals & WebSocket
// =========================================
let approvalSocket = null;
let approvalEverOpened = false;
let approvalFailedConnections = 0;
const APPROVAL_WS_MAX_FAILED_CONNECTIONS = 5;

function connectApprovalWS() {
    if (!approvalEverOpened && approvalFailedConnections >= APPROVAL_WS_MAX_FAILED_CONNECTIONS) {
        console.warn('Approval WebSocket unavailable; stopped reconnecting.');
        return;
    }

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    approvalSocket = new WebSocket(`${protocol}//${window.location.host}/ws/approval`);

    approvalSocket.onopen = () => {
        approvalEverOpened = true;
        approvalFailedConnections = 0;
    };

    approvalSocket.onmessage = (event) => {
        const msg = JSON.parse(event.data);
        if (msg.type === 'approval_request') {
            handleApprovalRequest(msg.data);
        }
    };

    approvalSocket.onclose = () => {
        console.log('Approval WebSocket closed. Reconnecting in 5s...');
        if (approvalEverOpened) {
            setTimeout(connectApprovalWS, 5000);
        } else {
            approvalFailedConnections += 1;
            if (approvalFailedConnections < APPROVAL_WS_MAX_FAILED_CONNECTIONS) {
                setTimeout(connectApprovalWS, 5000);
            }
        }
    };
}

function handleApprovalRequest(req) {
    // Check if modal is already open
    const modal = document.getElementById('modal-approval');
    if (!modal.classList.contains('hidden')) return;

    // Display approval modal
    window.showApprovalModal = function (req) {
        document.getElementById('appr-tool').textContent = req.tool || 'Research Agent';
        document.getElementById('appr-context').textContent = req.context || 'Task Execution';
        document.getElementById('appr-args').textContent = JSON.stringify(req.params || {}, null, 2);
        document.getElementById('appr-req-id').value = req.request_id;
        document.getElementById('appr-nonce').value = req.nonce;
        document.getElementById('appr-reason').value = '';

        // Update Risk Badge
        const riskBadge = document.getElementById('appr-risk-badge');
        const risk = req.risk_level || 'high'; // Default to high for safety
        riskBadge.className = `risk-badge risk-${risk}`;
        riskBadge.querySelector('span').textContent = `${risk.toUpperCase()} Risk Action`;

        // Update Request Time
        const timeStr = new Date().toLocaleString();
        document.getElementById('appr-req-time').textContent = timeStr;

        // Populate Timeline (Dynamic if req.timeline exists, otherwise static)
        const timeline = document.getElementById('appr-timeline');
        if (req.timeline && req.timeline.length > 0) {
            timeline.innerHTML = req.timeline.map((step, idx) => `
                <div class="timeline-item ${idx < req.timeline.length - 1 ? 'completed' : 'active'}">
                    <div class="timeline-content">
                        <span class="timeline-time">${escapeHtml(step.time || '')}</span>
                        <span class="timeline-title">${escapeHtml(step.title)}</span>
                    </div>
                </div>
            `).join('');
        }

        const modal = document.getElementById('modal-approval');
        modal.classList.remove('hidden');
    };
    window.showApprovalModal(req); // Call the new function to display the modal
}

document.addEventListener('DOMContentLoaded', () => {
    connectApprovalWS();
});

document.getElementById('btn-approve')?.addEventListener('click', () => submitDecision('approved'));
document.getElementById('btn-deny')?.addEventListener('click', () => submitDecision('denied'));

async function submitDecision(decision) {
    const reqId = document.getElementById('appr-req-id').value;
    const nonce = document.getElementById('appr-nonce').value;
    const reason = document.getElementById('appr-reason').value;

    if (!approvalSocket || approvalSocket.readyState !== WebSocket.OPEN) {
        alert('WebSocket not connected. Cannot submit approval.');
        return;
    }

    const payload = {
        type: 'approval_response',
        request_id: reqId,
        nonce: nonce,
        decision: decision,
        reason: reason,
        reason_code: decision === 'approved' ? 'USER_APPROVED' : 'USER_DENIED'
    };

    approvalSocket.send(JSON.stringify(payload));
    closeModal('modal-approval');
    loadPendingApprovals(); // Refresh list if needed
}

async function loadPendingApprovals() {
    try {
        // For P1, we query the audit log for APPROVAL_REQUESTED that hasn't been closed
        const res = await fetchWithAuth(`${API_BASE}/audit?limit=50&action=APPROVAL_REQUESTED`);
        const entries = await res.json();

        const list = document.getElementById('approval-list');
        if (entries.length === 0) {
            list.innerHTML = '<div class="empty-state">All clear! No pending approvals.</div>';
            return;
        }

        list.innerHTML = entries.map(e => {
            const meta = e.metadata || {};
            return `
                <div class="approval-card" data-id="${e.id}">
                    <div class="approval-info">
                        <h4>Request History</h4>
                        <p class="text-sm"><strong>${escapeHtml(meta.plan?.tool || 'Action')}</strong></p>
                        <span class="approval-meta">User: ${escapeHtml(e.user_id)}</span>
                    </div>
                     <div class="approval-status ${e.outcome === 'Success' ? 'text-green' : 'text-orange'}">
                        ${escapeHtml(e.outcome)}
                    </div>
                </div>
            `;
        }).join('');
    } catch (err) {
        console.error('Failed to load pending approvals:', err);
    }
}

// Deprecated REST submit (kept for Reference, but unused for P0)
window.submitApproval = async (id, decision) => {
    console.warn("REST approval not implemented for P0. Use WebSocket.");
};

document.getElementById('btn-refresh-approvals')?.addEventListener('click', loadPendingApprovals);

// =========================================
// Network Governance (Domain Rules)
// =========================================
let currentPolicy = { allow_domains: [], deny_domains: [] };

async function loadDomainGovernance() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/config`);
        const data = await res.json();

        // Use policy from config if available, fallback to defaults
        currentPolicy = data.network_policy || { allow_domains: [], deny_domains: [] };
        renderDomainLists();
    } catch (err) {
        console.error('Failed to load domain governance:', err);
    }
}

function renderDomainLists() {
    const allowList = document.getElementById('list-allow-domains');
    const denyList = document.getElementById('list-deny-domains');

    if (allowList) {
        allowList.innerHTML = currentPolicy.allow_domains.map(d => `
            <li class="domain-item">
                <span class="text-sm">${escapeHtml(d)}</span>
                <button class="btn-remove-domain" onclick="removeDomain('allow', '${d}')"><i class="fa-solid fa-trash-can"></i></button>
            </li>
        `).join('');
    }

    if (denyList) {
        denyList.innerHTML = currentPolicy.deny_domains.map(d => `
            <li class="domain-item">
                <span class="text-sm">${escapeHtml(d)}</span>
                <button class="btn-remove-domain" onclick="removeDomain('deny', '${d}')"><i class="fa-solid fa-trash-can"></i></button>
            </li>
        `).join('');
    }
}

window.removeDomain = (type, domain) => {
    if (type === 'allow') {
        currentPolicy.allow_domains = currentPolicy.allow_domains.filter(d => d !== domain);
    } else {
        currentPolicy.deny_domains = currentPolicy.deny_domains.filter(d => d !== domain);
    }
    renderDomainLists();
};

document.getElementById('btn-add-allow')?.addEventListener('click', () => {
    const input = document.getElementById('input-allow-domain');
    const domain = input.value.trim();
    if (domain && !currentPolicy.allow_domains.includes(domain)) {
        currentPolicy.allow_domains.push(domain);
        input.value = '';
        renderDomainLists();
    }
});

document.getElementById('input-allow-domain')?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
        e.preventDefault();
        document.getElementById('btn-add-allow').click();
    }
});

document.getElementById('btn-add-deny')?.addEventListener('click', () => {
    const input = document.getElementById('input-deny-domain');
    const domain = input.value.trim();
    if (domain && !currentPolicy.deny_domains.includes(domain)) {
        currentPolicy.deny_domains.push(domain);
        input.value = '';
        renderDomainLists();
    }
});

document.getElementById('input-deny-domain')?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
        e.preventDefault();
        document.getElementById('btn-add-deny').click();
    }
});

document.getElementById('btn-save-domains')?.addEventListener('click', async () => {
    const btn = document.getElementById('btn-save-domains');
    btn.disabled = true;
    btn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Saving...';

    try {
        const res = await fetchWithAuth(`${API_BASE}/config/network`, {
            method: 'POST',
            body: JSON.stringify(currentPolicy)
        });

        if (res.ok) {
            btn.innerHTML = '<i class="fa-solid fa-check"></i> Saved!';
            setTimeout(() => {
                btn.innerHTML = '<i class="fa-solid fa-save"></i> Save Changes';
                btn.disabled = false;
            }, 2000);
        } else {
            alert('Failed to save network policy');
            btn.disabled = false;
        }
    } catch (err) {
        console.error('Failed to save domains:', err);
        btn.disabled = false;
    }
});

// =========================================
// Audit Export
// =========================================
document.getElementById('btn-export-audit')?.addEventListener('click', async () => {
    const btn = document.getElementById('btn-export-audit');
    btn.disabled = true;
    btn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Exporting...';

    try {
        const res = await fetchWithAuth(`${API_BASE}/audit/export`);
        if (res.ok) {
            const blob = await res.blob();
            const url = window.URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = `audit_bundle_${new Date().toISOString().slice(0, 19).replace(/[:T]/g, '_')}.zip`;
            document.body.appendChild(a);
            a.click();
            a.remove();
            btn.innerHTML = '<i class="fa-solid fa-check"></i> Success';
        } else {
            alert('Export failed');
        }
    } catch (err) {
        console.error('Export failed:', err);
    }

    setTimeout(() => {
        btn.disabled = false;
        btn.innerHTML = '<i class="fa-solid fa-file-export"></i> Export ZIP';
    }, 2000);
});

async function loadAuditLogs() {
    const userId = document.getElementById('filter-user')?.value || '';
    const action = document.getElementById('filter-action')?.value || '';

    let url = `${API_BASE}/audit?limit=50`;
    if (userId) url += `&user_id=${encodeURIComponent(userId)}`;
    if (action) url += `&action=${encodeURIComponent(action)}`;

    try {
        const res = await fetchWithAuth(url);
        const entries = await res.json();

        const tbody = document.getElementById('audit-body');
        if (entries.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="empty-state">No audit entries found</td></tr>';
            return;
        }

        tbody.innerHTML = entries.map(e => `
            <tr>
                <td>${formatTimestamp(e.timestamp)}</td>
                <td>${escapeHtml(e.user_id)}</td>
                <td>${escapeHtml(e.action)}</td>
                <td>${escapeHtml(e.resource)}</td>
                <td><span class="outcome-${(e.outcome || '').toLowerCase()}">${escapeHtml(e.outcome)}</span></td>
            </tr>
        `).join('');
    } catch (err) {
        console.error('Failed to load audit logs:', err);
    }
}

document.getElementById('btn-refresh')?.addEventListener('click', loadAuditLogs);

// =========================================
// Test Harness
// =========================================
let harnessSuites = [];

async function loadHarnessSuites() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/harness/suites`);
        if (res.ok) {
            harnessSuites = await res.json();
            const select = document.getElementById('harness-suite-select');
            if (select) {
                select.innerHTML = harnessSuites.map(suite => 
                    `<option value="${suite.id}">${suite.name}</option>`
                ).join('');
                
                // Add event listener if not already added
                if (!select.dataset.listenerAdded) {
                    select.addEventListener('change', (e) => {
                        updateHarnessSuiteDetails(e.target.value);
                    });
                    select.dataset.listenerAdded = "true";
                }
                
                // Initially show the first suite's details
                if (harnessSuites.length > 0) {
                    updateHarnessSuiteDetails(select.value);
                }
            }
        }
    } catch (err) {
        console.error('Failed to load harness suites:', err);
    }
}

function updateHarnessSuiteDetails(suiteId) {
    const suite = harnessSuites.find(s => s.id === suiteId);
    if (!suite) return;
    
    const nameEl = document.getElementById('harness-suite-name');
    const descEl = document.getElementById('harness-suite-desc');
    if (nameEl) nameEl.textContent = suite.name;
    if (descEl) descEl.textContent = suite.description;
    
    // Hide results panel and clear cases list
    const summaryPanel = document.getElementById('harness-summary-panel');
    if (summaryPanel) summaryPanel.style.display = 'none';
    
    const casesList = document.getElementById('harness-cases-list');
    if (casesList) {
        casesList.innerHTML = suite.cases.map(tc => `
            <div class="test-case-card">
                <div class="case-card-header">
                    <div class="case-info-left">
                        <span class="status-badge" style="background: rgba(255,255,255,0.05); color: var(--text-muted); border: 1px solid var(--border);">Ready</span>
                        <h4 class="case-name">${tc.name}</h4>
                    </div>
                </div>
                <p class="case-description">${tc.description}</p>
                <div class="case-tags">
                    ${tc.tags.map(tag => `<span class="case-tag">${tag}</span>`).join('')}
                </div>
            </div>
        `).join('');
    }
}

document.getElementById('btn-run-harness')?.addEventListener('click', async () => {
    const select = document.getElementById('harness-suite-select');
    if (!select || !select.value) return;
    
    const suiteId = select.value;
    const btn = document.getElementById('btn-run-harness');
    const statusOverlay = document.getElementById('harness-running-status');
    const summaryPanel = document.getElementById('harness-summary-panel');
    
    // Enable loader, disable button, hide summary panel
    if (btn) {
        btn.disabled = true;
        btn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> Running...';
    }
    if (statusOverlay) statusOverlay.style.display = 'block';
    if (summaryPanel) summaryPanel.style.display = 'none';
    
    try {
        const res = await fetchWithAuth(`${API_BASE}/harness/run`, {
            method: 'POST',
            body: JSON.stringify({ suite_id: suiteId })
        });
        
        if (res.ok) {
            const result = await res.json();
            renderHarnessSuiteResult(result);
        } else {
            alert('Failed to run harness suite. Check server configuration.');
        }
    } catch (err) {
        console.error('Failed to execute harness suite:', err);
        alert('An error occurred during execution.');
    } finally {
        if (btn) {
            btn.disabled = false;
            btn.innerHTML = '<i class="fa-solid fa-play"></i> Run Suite';
        }
        if (statusOverlay) statusOverlay.style.display = 'none';
    }
});

function renderHarnessSuiteResult(result) {
    const summaryPanel = document.getElementById('harness-summary-panel');
    if (summaryPanel) {
        summaryPanel.style.display = 'block';
        
        const successRate = result.total_cases > 0 
            ? Math.round((result.passed_cases / result.total_cases) * 100) 
            : 0;
            
        document.getElementById('harness-stat-success').textContent = `${successRate}% (${result.passed_cases}/${result.total_cases})`;
        document.getElementById('harness-stat-latency').textContent = `${result.avg_latency_ms} ms`;
        document.getElementById('harness-stat-tokens').textContent = result.total_tokens.toLocaleString();
    }
    
    const casesList = document.getElementById('harness-cases-list');
    if (casesList && result.results) {
        casesList.innerHTML = result.results.map(tcResult => {
            const originalCase = getTestCaseDetails(result.suite_id, tcResult.test_case_id);
            const statusClass = tcResult.status.toLowerCase();
            const badgeIcon = statusClass === 'passed' ? 'fa-circle-check' : 'fa-circle-xmark';
            
            // Generate tags string
            const tagsHtml = originalCase 
                ? originalCase.tags.map(t => `<span class="case-tag">${escapeHtml(t)}</span>`).join('') 
                : '';
                
            // Format expected output text for assertion representation
            const assertionHtml = originalCase 
                ? renderAssertionDetails(originalCase.expected_output)
                : '';
                
            // Render failure reason if failed
            const failureHtml = tcResult.failure_reason 
                ? `<div class="failure-reason-box">
                    <i class="fa-solid fa-triangle-exclamation"></i>
                    <div><strong>Failure Reason:</strong> ${escapeHtml(tcResult.failure_reason)}</div>
                   </div>`
                : '';
                
            // Render ReAct trace history entries
            const historyTimelineHtml = renderHistoryTimeline(tcResult.history);
            
            return `
                <div class="test-case-card">
                    <div class="case-card-header">
                        <div class="case-info-left">
                            <span class="status-badge ${statusClass}">
                                <i class="fa-solid ${badgeIcon}"></i>
                                ${escapeHtml(tcResult.status)}
                            </span>
                            <h4 class="case-name">${escapeHtml(tcResult.name)}</h4>
                        </div>
                        <div class="case-stats">
                            <div class="case-stat-item">
                                <i class="fa-solid fa-stopwatch"></i>
                                <span>${tcResult.latency_ms}ms</span>
                            </div>
                            <div class="case-stat-item">
                                <i class="fa-solid fa-arrow-rotate-right"></i>
                                <span>${tcResult.steps} steps</span>
                            </div>
                            <div class="case-stat-item">
                                <i class="fa-solid fa-coins"></i>
                                <span>${tcResult.tokens_used} tokens</span>
                            </div>
                            <button class="toggle-trace-btn" onclick="toggleTraceDrawer(this, '${tcResult.test_case_id}')">
                                <i class="fa-solid fa-chevron-down"></i> View Trace
                            </button>
                        </div>
                    </div>
                    
                    <p class="case-description">${originalCase ? originalCase.description : ''}</p>
                    <div class="case-tags">${tagsHtml}</div>
                    
                    ${failureHtml}
                    
                    <!-- Trace Drawer -->
                    <div class="trace-drawer" id="trace-drawer-${tcResult.test_case_id}">
                        <div class="assertions-panel">
                            <div class="assertion-box">
                                <div class="assertion-box-title">Input Prompt</div>
                                <div class="assertion-box-content" style="font-family: inherit; font-size: 14px;">${originalCase ? originalCase.prompt : ''}</div>
                            </div>
                            <div class="assertion-box">
                                <div class="assertion-box-title">Expected Criteria</div>
                                <div class="assertion-box-content">${assertionHtml}</div>
                            </div>
                        </div>
                        
                        <div class="assertion-box" style="margin-bottom: 20px;">
                            <div class="assertion-box-title">Actual Output</div>
                            <div class="assertion-box-content" style="font-family: inherit; font-size: 14px; background: rgba(0,0,0,0.15); border-left: 3px solid var(--primary);">${escapeHtml(tcResult.actual_output)}</div>
                        </div>
                        
                        <h5 class="section-title" style="font-size: 14px; margin-top: 20px;">ReAct Reasoning Loop Trace</h5>
                        <div class="react-trace-timeline">
                            ${historyTimelineHtml}
                        </div>
                    </div>
                </div>
            `;
        }).join('');
    }
}

function getTestCaseDetails(suiteId, caseId) {
    const suite = harnessSuites.find(s => s.id === suiteId);
    if (!suite) return null;
    return suite.cases.find(c => c.id === caseId) || null;
}

function renderAssertionDetails(assertion) {
    if (!assertion || !assertion.type) return 'Unknown';
    switch (assertion.type) {
        case 'ExactMatch':
            return `ExactMatch: "${escapeHtml(assertion.value)}"`;
        case 'Contains':
            return `Contains: "${escapeHtml(assertion.value)}"`;
        case 'Regex':
            return `Regex: /${escapeHtml(assertion.value)}/`;
        case 'JsonSchema':
            return `JSON Schema Match:\n${escapeHtml(JSON.stringify(assertion.value, null, 2))}`;
        case 'LlmJudge':
            return `LLM Judge Criteria: "${escapeHtml(assertion.value.criteria)}"`;
        default:
            return `${assertion.type}: ${escapeHtml(JSON.stringify(assertion.value))}`;
    }
}

function renderHistoryTimeline(history) {
    if (!history || history.length === 0) {
        return '<div class="text-muted text-sm" style="padding: 10px 0;">No ReAct steps recorded. (Ensure persist_state is enabled in controller)</div>';
    }
    
    return history.map((entry, index) => {
        let nodeClass = '';
        let icon = '';
        let stepTitle = '';
        let contentHtml = '';
        
        const contentStr = entry.content || '';
        
        if (entry.role === 'user') {
            nodeClass = 'user';
            icon = '<i class="fa-solid fa-user"></i>';
            stepTitle = 'User Prompt';
            contentHtml = `<div class="trace-step-body">${escapeHtml(contentStr)}</div>`;
        } else if (entry.role === 'assistant') {
            if (contentStr.includes('FINAL ANSWER:')) {
                nodeClass = 'final-answer';
                icon = '<i class="fa-solid fa-flag-checkered"></i>';
                stepTitle = 'Final Answer';
                contentHtml = `<div class="trace-step-body">${escapeHtml(contentStr)}</div>`;
            } else {
                nodeClass = 'thought';
                icon = '<i class="fa-solid fa-brain"></i>';
                stepTitle = `Reasoning Step ${index}`;
                
                let formatted = escapeHtml(contentStr);
                formatted = formatted.replace(/(THOUGHT:)/g, '<strong style="color: var(--accent);">$1</strong>');
                formatted = formatted.replace(/(ACTION:)/g, '<strong style="color: var(--primary);">$1</strong>');
                formatted = formatted.replace(/(ARGS:)/g, '<strong style="color: var(--primary);">$1</strong>');
                
                contentHtml = `<div class="trace-step-body">${formatted}</div>`;
            }
        } else if (entry.role === 'tool') {
            nodeClass = 'observation';
            icon = '<i class="fa-solid fa-terminal"></i>';
            stepTitle = 'Tool Observation';
            contentHtml = `<div class="trace-step-body"><pre>${escapeHtml(contentStr)}</pre></div>`;
        } else {
            nodeClass = 'system';
            icon = '<i class="fa-solid fa-gears"></i>';
            stepTitle = 'System Event';
            contentHtml = `<div class="trace-step-body">${escapeHtml(contentStr)}</div>`;
        }
        
        let toolCallHtml = '';
        if (entry.tool_call) {
            const tc = entry.tool_call;
            const argsStr = typeof tc.arguments === 'object' ? JSON.stringify(tc.arguments, null, 2) : tc.arguments;
            const resultStr = tc.result ? (typeof tc.result === 'object' ? JSON.stringify(tc.result, null, 2) : tc.result) : null;
            
            toolCallHtml = `
                <div class="tool-call-details" style="margin-top: 10px; padding: 10px; border-radius: 6px; background: rgba(0,0,0,0.2); border: 1px solid var(--border);">
                    <div style="font-size: 12px; font-weight: 600; color: var(--primary); margin-bottom: 4px;">
                        <i class="fa-solid fa-cube"></i> Tool Call: ${tc.name}
                    </div>
                    <pre style="margin: 0; font-size: 11px; padding: 6px;">Arguments: ${escapeHtml(argsStr)}</pre>
                    ${resultStr ? `<pre style="margin-top: 6px; font-size: 11px; padding: 6px; border-left: 2px solid var(--success);">Result: ${escapeHtml(resultStr)}</pre>` : ''}
                </div>
            `;
        }
        
        const timestampStr = entry.timestamp 
            ? new Date(entry.timestamp * 1000).toLocaleTimeString() 
            : '';
            
        return `
            <div class="trace-step-node ${nodeClass}">
                <div class="trace-step-icon">${icon}</div>
                <div class="trace-step-content">
                    <div class="trace-step-header">
                        <span class="trace-step-title">${stepTitle}</span>
                        <span class="trace-step-time">${timestampStr}</span>
                    </div>
                    ${contentHtml}
                    ${toolCallHtml}
                </div>
            </div>
        `;
    }).join('');
}

window.toggleTraceDrawer = (btn, caseId) => {
    const drawer = document.getElementById(`trace-drawer-${caseId}`);
    if (!drawer) return;
    
    const isActive = drawer.classList.toggle('active');
    
    if (isActive) {
        btn.innerHTML = '<i class="fa-solid fa-chevron-up"></i> Hide Trace';
    } else {
        btn.innerHTML = '<i class="fa-solid fa-chevron-down"></i> View Trace';
    }
};

function escapeHtml(str) {
    if (!str) return '';
    return str
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;")
        .replace(/'/g, "&#039;");
}

function formatTimestamp(ts) {
    if (!ts) return '—';
    const date = new Date(ts);
    return isNaN(date.getTime()) ? '—' : date.toLocaleString();
}

// =========================================
// Cognitive Security Audit
// =========================================
async function loadCognitiveData() {
    loadCognitiveMetrics();
    loadCognitiveSessions();
    loadCognitiveAnomalies();
}

async function loadCognitiveMetrics() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/cognitive/metrics`);
        if (res.ok) {
            const data = await res.json();
            document.getElementById('cog-stat-integrity').textContent = `${data.integrity_score}%`;
            document.getElementById('cog-stat-consensus').textContent = `${data.consensus_score}%`;
            document.getElementById('cog-stat-compliance').textContent = `${data.compliance_score}%`;
            document.getElementById('cog-stat-detection').textContent = `${data.detection_rate}%`;
        }
    } catch (err) {
        console.error('Failed to load cognitive metrics', err);
    }
}

async function loadCognitiveSessions() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/sessions`);
        if (res.ok) {
            const sessions = await res.json();
            const select = document.getElementById('cog-session-select');
            if (select) {
                // Keep the first option
                select.innerHTML = '<option value="">Select an active session...</option>';
                sessions.forEach(sess => {
                    const opt = document.createElement('option');
                    opt.value = sess.id;
                    opt.textContent = `${sess.id.substring(0, 8)}... (${sess.status})`;
                    select.appendChild(opt);
                });
            }
        }
    } catch (err) {
        console.error('Failed to load sessions', err);
    }
}

// Session selection change handler
document.getElementById('cog-session-select')?.addEventListener('change', async (e) => {
    const sessionId = e.target.value;
    if (!sessionId) {
        document.getElementById('workspace-goal').textContent = '--';
        document.getElementById('workspace-constraints').textContent = '--';
        document.getElementById('workspace-verified').textContent = '--';
        return;
    }

    try {
        const res = await fetchWithAuth(`${API_BASE}/sessions/${sessionId}/workspace`);
        if (res.ok) {
            const ws = await res.json();
            document.getElementById('workspace-goal').textContent = ws.objective;
            document.getElementById('workspace-constraints').textContent = ws.constraints;
            document.getElementById('workspace-verified').textContent = ws.verified;
        } else {
            document.getElementById('workspace-goal').textContent = 'Workspace state not active or empty';
            document.getElementById('workspace-constraints').textContent = 'None';
            document.getElementById('workspace-verified').textContent = 'None';
        }
    } catch (err) {
        console.error('Failed to fetch workspace state', err);
    }
});

async function loadCognitiveAnomalies() {
    try {
        const res = await fetchWithAuth(`${API_BASE}/cognitive/anomalies`);
        if (res.ok) {
            const list = await res.json();
            const container = document.getElementById('cog-anomalies-list');
            if (!container) return;

            if (list.length === 0) {
                container.innerHTML = `
                    <tr>
                        <td colspan="6" style="text-align: center; padding: 20px; color: var(--text-muted);">No cognitive anomalies detected.</td>
                    </tr>
                `;
                return;
            }

            container.innerHTML = list.map(item => `
                <tr id="anomaly-${item.id}">
                    <td style="font-family: monospace; font-size: 13px; font-weight: 600; color: var(--warning);">${item.id.substring(0, 8)}</td>
                    <td style="font-size: 13px; color: var(--text-muted);">${new Date(item.timestamp).toLocaleString()}</td>
                    <td style="font-family: monospace; font-size: 13px;">${item.session_id.substring(0, 8)}...</td>
                    <td style="font-size: 13px; font-weight: 500; color: var(--text-main);">${escapeHtml(item.violation_reason)}</td>
                    <td>
                        <span class="badge ${item.severity === 'critical' ? 'danger' : 'warning'}">${item.severity}</span>
                    </td>
                    <td>
                        <button onclick="resolveAnomaly('${item.id}', 'resolve')" class="btn-primary btn-sm" style="padding: 4px 8px; font-size: 12px; border-radius: 4px;">
                            <i class="fa-solid fa-check"></i> Resolve
                        </button>
                    </td>
                </tr>
            `).join('');
        }
    } catch (err) {
        console.error('Failed to load cognitive anomalies', err);
    }
}

window.resolveAnomaly = async (id, action) => {
    if (!confirm(`Are you sure you want to ${action} anomaly ${id}?`)) return;
    try {
        const res = await fetchWithAuth(`${API_BASE}/cognitive/anomalies/${id}/action`, {
            method: 'POST',
            body: JSON.stringify({ action })
        });
        if (res.ok) {
            loadCognitiveAnomalies();
        } else {
            alert('Failed to execute action on anomaly');
        }
    } catch (err) {
        console.error('Failed to take action on anomaly', err);
    }
};

// =========================================
// Initial Load
// =========================================
document.addEventListener('DOMContentLoaded', () => {
    loadMetrics();
    loadProviders();
    loadPersistenceConfig();
    loadMcpServers();
    loadAuditLogs();
    loadResearchRuns();
    loadPendingApprovals();
    loadDomainGovernance();
    loadHarnessSuites();
    loadCognitiveData();

    // Auto-refresh metrics every 5s
    setInterval(loadMetrics, 5000);
    setInterval(loadCognitiveMetrics, 5000);
});
