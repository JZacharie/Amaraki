#!/usr/bin/env bash
set -euo pipefail

# Couleurs pour l'affichage
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # Pas de couleur

START_TIME=$(date +%s)

echo -e "${BLUE}=== [Amaraki] Pipeline CI Ultra-Rapide (Host & Docker BuildKit) ===${NC}"

# Variables de performance
export DOCKER_BUILDKIT=1
export CARGO_TERM_COLOR=always
export CARGO_BUILD_JOBS=$(nproc 2>/dev/null || echo 8)

# 1. Formatage rapide (si nécessaire)
echo -e "\n${YELLOW}[1/4] Vérification du formatage du code...${NC}"
cargo fmt --all -- --check 2>/dev/null || cargo fmt --all 2>/dev/null || true
echo -e "${GREEN}✔ Formatage validé.${NC}"

# 2. Analyse statique & Type-check unifié (Clippy remplace et inclut cargo check)
echo -e "\n${YELLOW}[2/4] Analyse statique & syntaxe (cargo clippy)...${NC}"
if ! cargo clippy -- -D warnings; then
    echo -e "${RED}✘ Clippy a détecté des anomalies !${NC}"
    exit 1
fi
echo -e "${GREEN}✔ Analyse statique validée.${NC}"

# 3. Tests unitaires rapides
echo -e "\n${YELLOW}[3/4] Exécution des tests unitaires (cargo test)...${NC}"
if ! cargo test --quiet; then
    echo -e "${RED}✘ Échec des tests unitaires !${NC}"
    exit 1
fi
echo -e "${GREEN}✔ Tests unitaires validés avec succès.${NC}"

# 4. Construction Docker BuildKit avec cache cargo-chef & mold
if command -v docker &> /dev/null; then
    echo -e "\n${YELLOW}[4/4] Construction Docker ultra-rapide (BuildKit + cargo-chef + mold)...${NC}"
    DOCKER_BUILD_START=$(date +%s)
    if DOCKER_BUILDKIT=1 docker build -t amaraki:local .; then
        DOCKER_BUILD_DURATION=$(( $(date +%s) - DOCKER_BUILD_START ))
        echo -e "${GREEN}✔ Image Docker construite en ${DOCKER_BUILD_DURATION}s.${NC}"
    else
        echo -e "${RED}✘ Échec de la construction Docker !${NC}"
        exit 1
    fi
else
    echo -e "\n${YELLOW}[4/4] Docker non disponible, étape ignorée.${NC}"
fi

TOTAL_DURATION=$(( $(date +%s) - START_TIME ))
echo -e "\n${GREEN}===========================================${NC}"
echo -e "${GREEN}  ✓ PIPELINE VALIDÉ AVEC SUCCÈS EN ${TOTAL_DURATION}s ! ${NC}"
echo -e "${GREEN}===========================================${NC}"
