//! Renders a sample print job to `sample.pdf` for eyeballing the layout.
//! The content is long enough to span two pages, so the repeated heading and
//! the per-page footer are both visible.
use webprint_pdf::{PrintJob, Renderer};

fn main() {
    let content = r#"#include <bits/stdc++.h>
using namespace std;

// Dijkstra over an adjacency list, plus a few helpers, so this job spans
// more than one page. Reads n, m then m edges, then q queries.
const long long INF = 1e18;

struct Edge {
    int to;
    long long weight;
};

// Reads a whitespace-separated integer by hand: faster than cin on big inputs.
template <typename T>
T read_int() {
    T value = 0;
    int sign = 1, c = getchar();
    while (c != '-' && (c < '0' || c > '9')) c = getchar();
    if (c == '-') { sign = -1; c = getchar(); }
    while (c >= '0' && c <= '9') {
        value = value * 10 + (c - '0');
        c = getchar();
    }
    return value * sign;
}

vector<long long> dijkstra(const vector<vector<Edge>>& g, int source) {
    vector<long long> dist(g.size(), INF);
    priority_queue<pair<long long, int>, vector<pair<long long, int>>, greater<>> pq;

    dist[source] = 0;
    pq.push({0, source});
    while (!pq.empty()) {
        auto [d, u] = pq.top();
        pq.pop();
        if (d > dist[u]) continue;          /* a stale entry */
        for (const Edge& e : g[u]) {
            if (dist[u] + e.weight < dist[e.to]) {
                dist[e.to] = dist[u] + e.weight;
                pq.push({dist[e.to], e.to});
            }
        }
    }
    return dist;
}

// Union-find with path compression, used for the second subtask.
struct DSU {
    vector<int> parent, rank_;

    explicit DSU(int n) : parent(n), rank_(n, 0) {
        iota(parent.begin(), parent.end(), 0);
    }

    int find(int x) {
        return parent[x] == x ? x : parent[x] = find(parent[x]);
    }

    bool unite(int a, int b) {
        a = find(a);
        b = find(b);
        if (a == b) return false;
        if (rank_[a] < rank_[b]) swap(a, b);
        parent[b] = a;
        if (rank_[a] == rank_[b]) rank_[a]++;
        return true;
    }
};

int main() {
    ios::sync_with_stdio(false);
    cin.tie(nullptr);

    int n = read_int<int>();
    int m = read_int<int>();
    vector<vector<Edge>> g(n + 1);
    DSU dsu(n + 1);

    for (int i = 0; i < m; ++i) {
        int u = read_int<int>();
        int v = read_int<int>();
        long long w = read_int<long long>();
        g[u].push_back({v, w});
        g[v].push_back({u, w});
        dsu.unite(u, v);
    }

    vector<long long> dist = dijkstra(g, 1);
    cout << (dist[n] == INF ? -1 : dist[n]) << "\n";

    int q = read_int<int>();
    while (q--) {
        int a = read_int<int>(), b = read_int<int>();
        cout << (dsu.find(a) == dsu.find(b) ? "YES" : "NO") << "\n";
    }
    return 0;
}
// A very long line to show wrapping behaviour: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
    let out = Renderer::with_highlighting().render(&PrintJob {
        team: "B052 Kazakh-British TU: Master ka4ka (Perdebay, Syrgabayev, Torekeldi)",
        site: "main-hall",
        label: "137",
        submitted_at: "2026-11-03 15:04:05",
        content,
        language: None,
    });
    std::fs::write("sample.pdf", &out.bytes).unwrap();
    println!(
        "pages={} lines={} language={:?} size={}KB",
        out.pages,
        out.lines,
        out.language,
        out.bytes.len() / 1024
    );
}
